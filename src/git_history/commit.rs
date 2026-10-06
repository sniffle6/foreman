//! The Git Changes window's writes (Add to VCS, Mark Resolved, Rollback,
//! Delete, Add to .gitignore, commit, push) and the commit panel pinned under
//! the tree (message box, Amend, Commit, Commit and Push…, and a one-click AI
//! draft of the message). Every write runs on a worker through `git::write`;
//! one at a time, so two never race for `index.lock`. A push takes no lock
//! and runs in its own slot beside them, cancellable.
use super::git::{self, GitError};
use super::push::{self, Target};
use super::toolbar::{self, Glyph, Tool};
use crate::ai_oneshot::{self, LaunchError};
use crate::config::NamingProvider;
use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;

/// Diff sent to the AI; over this, only `--stat` goes.
const DIFF_CAP: usize = 32 * 1024;
const AI_TIMEOUT: Duration = Duration::from_secs(90);
const AI_MAX_OUTPUT: usize = 16 * 1024;
pub(super) const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// More checked paths than this and the AI draft reads the whole diff.
const DRAFT_PATHS: usize = 500;
/// The notice shows at most this many lines, then scrolls.
const NOTICE_LINES: f32 = 5.0;
/// The push slot's spinner text and its Cancel hint.
const PUSHING: &str = "Pushing…";
const CANCEL_PUSH: &str = "Stop the push; the commit stays and can be pushed again";

const AI_RULES: &str = "You write Git commit messages. Reply with ONLY the commit \
message as plain text: a subject line of at most 72 characters, then optionally a \
blank line and a short body wrapped at 72 columns explaining why. Match the style \
of the recent commits shown (for example a `type(scope): subject` prefix). No code \
fences, no quotes around the message, no commentary before or after it.";

/// What a write does. Paths are as `git status` gave them: relative to the
/// window's directory.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Write {
    /// Add to VCS: `git add --intent-to-add`. Versioned, nothing staged.
    Track(Vec<String>),
    /// Mark Resolved: `git add`.
    Resolve(Vec<String>),
    /// Rollback: `untrack` (files new since HEAD) leave the index and become
    /// unversioned again, kept on disk; `restore` go back to HEAD, index and
    /// working copy both.
    Rollback {
        untrack: Vec<String>,
        restore: Vec<String>,
    },
    /// Delete: to the Recycle Bin.
    Delete(Vec<String>),
    /// Add to .gitignore: one anchored pattern per path.
    Ignore(Vec<String>),
    /// `git commit --only` of `paths` (their working-copy content); nothing
    /// else in the index goes in. `push`: open the push dialog after.
    /// `merge`: Git refuses a partial commit while merging, so `paths` are
    /// added and the whole index is committed.
    Commit {
        message: String,
        paths: Vec<String>,
        amend: bool,
        push: bool,
        merge: bool,
    },
    /// Push exactly where the push dialog showed.
    Push(Target),
}
impl Write {
    /// A write over no files does nothing; don't start it.
    pub(super) fn is_empty(&self) -> bool {
        match self {
            Self::Track(p) | Self::Resolve(p) | Self::Delete(p) | Self::Ignore(p) => p.is_empty(),
            Self::Rollback { untrack, restore } => untrack.is_empty() && restore.is_empty(),
            Self::Commit { .. } | Self::Push(_) => false,
        }
    }
    /// The spinner text while this holds the write slot, for the writes
    /// slow enough to show one. (The push slot has its own, `PUSHING`.)
    fn doing(&self) -> Option<&'static str> {
        match self {
            Self::Commit { push: true, .. } => Some("Committing, then push…"),
            Self::Commit { .. } => Some("Committing…"),
            _ => None,
        }
    }
}

/// A finished write, for the notice area (which shows `text`, in red when
/// `error`, and ignores the rest).
#[derive(Debug, PartialEq)]
pub(super) struct Report {
    pub(super) text: String,
    pub(super) error: bool,
    /// A commit landed: clear the message.
    pub(super) committed: bool,
    /// ... and it was Commit and Push…: open the push dialog.
    pub(super) push: bool,
}
impl Report {
    fn new(result: Result<String, String>) -> Self {
        let (text, error) = match result {
            Ok(text) => (text, false),
            Err(text) => (text, true),
        };
        Self {
            text,
            error,
            committed: false,
            push: false,
        }
    }
}

/// `git --literal-pathspecs <args>` over `paths`, fed NUL-separated on stdin
/// so a long list never hits the command-line limit. No paths: no pathspec.
fn over_paths(cwd: &Path, args: &[&str], paths: &[String]) -> Result<String, GitError> {
    let mut all = vec!["--literal-pathspecs"];
    all.extend(args);
    if paths.is_empty() {
        return git::write(cwd, &all, None, None, None);
    }
    all.extend(["--pathspec-from-file=-", "--pathspec-file-nul"]);
    git::write(cwd, &all, Some(&paths.join("\0")), None, None)
}

/// Worker-only: run one write. `cancel` only reaches a push; every other
/// write runs to completion (see `git::write`).
pub(super) fn run(cwd: &Path, write: &Write, cancel: &Arc<AtomicBool>) -> Report {
    let quiet = |r: Result<String, GitError>, what| {
        Report::new(r.map(|_| String::new()).map_err(|e| failure(what, e)))
    };
    match write {
        Write::Track(paths) => quiet(
            over_paths(cwd, &["add", "--intent-to-add"], paths),
            "Cannot add to Git",
        ),
        Write::Resolve(paths) => quiet(over_paths(cwd, &["add"], paths), "Cannot mark resolved"),
        Write::Rollback { untrack, restore } => {
            // `reset` rather than `rm --cached`: it also works before the
            // first commit, and on intent-to-add entries.
            if !untrack.is_empty()
                && let Err(e) = over_paths(cwd, &["reset", "-q"], untrack)
            {
                return quiet(Err(e), "Rollback failed");
            }
            let args = ["restore", "--source=HEAD", "--staged", "--worktree"];
            if restore.is_empty() {
                return Report::new(Ok(String::new()));
            }
            quiet(over_paths(cwd, &args, restore), "Rollback failed")
        }
        Write::Delete(paths) => Report::new(recycle(cwd, paths)),
        Write::Ignore(paths) => Report::new(ignore(cwd, paths)),
        Write::Commit {
            message,
            paths,
            amend,
            push,
            merge,
        } => {
            if *merge
                && !paths.is_empty()
                && let Err(e) = over_paths(cwd, &["add"], paths)
            {
                return Report::new(Err(failure("Commit failed", e)));
            }
            let mut args = vec!["commit"];
            if *amend {
                args.push("--amend");
            }
            if !*merge {
                args.push("--only");
            }
            args.extend(["-m", message]);
            let paths: &[String] = if *merge { &[] } else { paths };
            let out = match over_paths(cwd, &args, paths) {
                Ok(out) => out,
                Err(e) => return Report::new(Err(failure("Commit failed", e))),
            };
            // "[main 1a2b3c4] subject"; hooks may print lines before it.
            let summary = out
                .lines()
                .find(|l| l.starts_with('['))
                .unwrap_or("Committed")
                .to_owned();
            Report {
                committed: true,
                push: *push,
                ..Report::new(Ok(summary))
            }
        }
        Write::Push(target) => Report::new(push::push(cwd, target, cancel)),
    }
}

pub(super) fn failure(what: &str, error: GitError) -> String {
    match error {
        GitError::Failed(text) if text.is_empty() => format!("{what}: Git exited with an error"),
        GitError::Failed(text) => format!("{what}:\n{text}"),
        other => format!("{what}: {other}"),
    }
}

/// A status path joined onto `cwd`, or `None` if it would leave it.
fn inside(cwd: &Path, path: &str) -> Option<PathBuf> {
    let relative = Path::new(path.trim_end_matches('/'));
    let normal = relative
        .components()
        .all(|c| matches!(c, std::path::Component::Normal(_)));
    (normal && !relative.as_os_str().is_empty()).then(|| cwd.join(relative))
}

/// Move `paths` to the Recycle Bin. On a drive without one, Windows deletes
/// them outright.
fn recycle(cwd: &Path, paths: &[String]) -> Result<String, String> {
    let mut full = Vec::new();
    for path in paths {
        let p = inside(cwd, path).ok_or_else(|| format!("Cannot delete {path}: invalid path"))?;
        full.push(p);
    }
    to_recycle_bin(&full).map(|()| String::new())
}

#[cfg(windows)]
fn to_recycle_bin(paths: &[PathBuf]) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::{
        FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, SHFILEOPSTRUCTW,
        SHFileOperationW,
    };
    // Double-NUL-terminated list; the shell wants backslashes.
    let mut from: Vec<u16> = Vec::new();
    for path in paths {
        let path = path.to_string_lossy().replace('/', "\\");
        from.extend(std::ffi::OsStr::new(&path).encode_wide());
        from.push(0);
    }
    from.push(0);
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: from.as_ptr(),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT) as u16,
        ..Default::default()
    };
    // SAFETY: `op` and the `from` buffer it points into outlive the call.
    let code = unsafe { SHFileOperationW(&mut op) };
    if code != 0 {
        return Err(format!("Delete failed (shell error {code:#x})"));
    }
    if op.fAnyOperationsAborted != 0 {
        return Err("Delete was cancelled".into());
    }
    Ok(())
}

#[cfg(not(windows))]
fn to_recycle_bin(_paths: &[PathBuf]) -> Result<(), String> {
    Err("Deleting to the trash is only supported on Windows".into())
}

/// A `.gitignore` line matching exactly `path`: anchored with `/`, with
/// glob characters, a backslash, and trailing spaces escaped.
pub(super) fn ignore_pattern(path: &str) -> String {
    let mut out = String::from("/");
    let (body, dir) = match path.strip_suffix('/') {
        Some(body) => (body, "/"),
        None => (path, ""),
    };
    let keep = body.trim_end_matches(' ').len();
    for (i, c) in body.char_indices() {
        if matches!(c, '*' | '?' | '[' | '\\') || (c == ' ' && i >= keep) {
            out.push('\\');
        }
        out.push(c);
    }
    out.push_str(dir);
    out
}

/// Append a pattern per path to the `.gitignore` in `cwd`.
fn ignore(cwd: &Path, paths: &[String]) -> Result<String, String> {
    if let Some(bad) = paths.iter().find(|p| inside(cwd, p).is_none()) {
        return Err(format!("Cannot ignore {bad}: invalid path"));
    }
    let file = cwd.join(".gitignore");
    let mut text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("Cannot read .gitignore: {e}")),
    };
    // Keep the file's own line ending.
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    if !text.is_empty() && !text.ends_with('\n') {
        text.push_str(eol);
    }
    for path in paths {
        text.push_str(&ignore_pattern(path));
        text.push_str(eol);
    }
    std::fs::write(&file, text).map_err(|e| format!("Cannot write .gitignore: {e}"))?;
    Ok(String::new())
}

/// Worker-only: the prompt for an AI draft, from the diff against HEAD of
/// `paths` (all changes when `None`), or its `--stat` when too large, and
/// recent subjects for style.
fn draft_prompt(
    cwd: &Path,
    paths: Option<&[String]>,
    cancel: &Arc<AtomicBool>,
) -> Result<String, String> {
    let read = |args: &[&str], cap| git::output(cwd, args, cancel, cap, READ_TIMEOUT);
    let born = read(&["rev-parse", "--verify", "-q", "HEAD"], 4096).is_ok();
    // Before the first commit there is no HEAD: staged adds, then
    // intent-to-add files (which only the index-to-worktree diff shows).
    let bases: &[&[&str]] = if born {
        &[&["HEAD"]]
    } else {
        &[&["--cached"], &[]]
    };
    let diff = |extra: &[&str], cap| -> Result<String, GitError> {
        let mut text = String::new();
        for base in bases {
            let mut args = vec!["diff", "--no-color", "--no-ext-diff", "-M"];
            args.extend(*base);
            args.extend(extra);
            if let Some(paths) = paths.filter(|p| p.len() <= DRAFT_PATHS) {
                args.push("--");
                args.extend(paths.iter().map(String::as_str));
            }
            text.push_str(&String::from_utf8_lossy(&read(&args, cap)?));
            if text.len() > cap {
                return Err(GitError::TooLarge);
            }
        }
        Ok(text)
    };
    let diff = match diff(&[], DIFF_CAP) {
        Ok(text) => text,
        Err(GitError::TooLarge) => {
            let mut stat = diff(&["--stat=200"], 4 << 20)
                .map_err(|e| format!("Cannot read the changes: {e}"))?;
            truncate(&mut stat, DIFF_CAP);
            format!("(The full diff is too large; this is its summary.)\n{stat}")
        }
        Err(e) => return Err(format!("Cannot read the changes: {e}")),
    };
    if diff.trim().is_empty() {
        return Err("Nothing to commit".into());
    }
    // A repository with no commits yet has no log; that's fine.
    let log = read(&["log", "--oneline", "--no-color", "-10"], 64 * 1024)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    Ok(format!(
        "Recent commits, for style:\n<recent_commits>\n{log}</recent_commits>\n\n\
         Changes to describe:\n<diff>\n{diff}\n</diff>"
    ))
}

fn truncate(text: &mut String, cap: usize) {
    if text.len() > cap {
        let mut end = cap;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
}

/// The AI's reply as a commit message, or why it is unusable. Untrusted:
/// it must be plain text, not a fenced block or an empty shrug.
pub(super) fn clean_message(raw: &str) -> Result<String, String> {
    let text = raw.replace("\r\n", "\n");
    let text = text.trim();
    if text.is_empty() {
        return Err("The AI returned an empty message".into());
    }
    if text.contains("```") {
        return Err("The AI returned a code block instead of a message".into());
    }
    if text
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err("The AI returned control characters".into());
    }
    Ok(text.to_owned())
}

fn draft(
    cwd: &Path,
    paths: Option<&[String]>,
    provider: NamingProvider,
    model: &str,
    cancel: &Arc<AtomicBool>,
) -> Result<String, String> {
    let prompt = draft_prompt(cwd, paths, cancel)?;
    // The CLI runs outside the repository: the diff is all it needs.
    let scratch = crate::config::config_dir()
        .ok_or("No settings directory")?
        .join("commit-message");
    std::fs::create_dir_all(&scratch).map_err(|_| "Cannot prepare the AI run".to_string())?;
    let program = ai_oneshot::program(provider);
    let raw = ai_oneshot::run(&ai_oneshot::Request {
        provider,
        model,
        system: Some(AI_RULES),
        prompt: &prompt,
        cwd: &scratch,
        timeout: AI_TIMEOUT,
        max_output: AI_MAX_OUTPUT,
    })
    .map_err(|e| match e {
        LaunchError::Unavailable => format!("{program} CLI is unavailable"),
        LaunchError::Failed => format!("{program} could not write a message"),
        LaunchError::Timeout => format!("{program} timed out"),
        LaunchError::TooLarge => format!("{program} replied with too much text"),
    })?;
    clean_message(&raw)
}

/// Worker-only: the last commit's message, for Amend.
fn last_message(cwd: &Path, cancel: &Arc<AtomicBool>) -> Result<String, String> {
    let bytes = git::output(
        cwd,
        &["log", "-1", "--format=%B"],
        cancel,
        64 * 1024,
        READ_TIMEOUT,
    )
    .map_err(|e| format!("Cannot read the last commit: {e}"))?;
    Ok(String::from_utf8_lossy(&bytes).trim_end().to_owned())
}

/// A worker's single reply; dropping it cancels (reads and the push) or
/// just stops listening (the other writes, which always run to completion).
pub(super) struct Job<T> {
    receiver: mpsc::Receiver<T>,
    cancel: Arc<AtomicBool>,
}
impl<T: Send + 'static> Job<T> {
    pub(super) fn start(
        ctx: &egui::Context,
        work: impl FnOnce(&Arc<AtomicBool>) -> T + Send + 'static,
    ) -> Self {
        let (tx, receiver) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = work(&stop);
            if !stop.load(Ordering::Relaxed) {
                let _ = tx.send(result);
                ctx.request_repaint();
            }
        });
        Self { receiver, cancel }
    }
    /// `Some` once, when the worker replies (or dies).
    pub(super) fn poll(&self) -> Option<Option<T>> {
        match self.receiver.try_recv() {
            Ok(v) => Some(Some(v)),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(None),
        }
    }
}
impl<T> Drop for Job<T> {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// A write in flight and the spinner text it shows, fixed when it started.
struct Running {
    job: Job<Report>,
    doing: Option<&'static str>,
}

/// What the next commit takes, as the Changes window sees it.
#[derive(Default)]
pub(super) struct Scope {
    /// The checked Changes files, or all of them when none is checked; a
    /// rename brings both of its paths.
    pub(super) paths: Vec<String>,
    /// How many files that is.
    pub(super) files: usize,
    /// Some file is checked (otherwise Commit takes everything).
    pub(super) checked: bool,
    /// A merge has unresolved files: no commit until they're resolved.
    pub(super) conflicts: bool,
    /// A merge is in progress: Commit takes every change, checked or not.
    pub(super) merging: bool,
    /// No commit yet, so nothing to amend.
    pub(super) unborn: bool,
}

/// What the panel did this frame, for the Changes window to act on.
#[derive(Default)]
pub(super) struct Outcome {
    /// A write finished: re-read the status (unless the watch will).
    pub(super) wrote: bool,
    /// Commit and Push… committed: open the push dialog.
    pub(super) push: bool,
}

/// Dropping the panel (closing the Changes window) drops its jobs: a
/// commit or `add` runs on to completion unheard, while the push's job
/// flag kills the push and its process tree. Safe: a killed push leaves
/// nothing locked locally, and the remote's ref update is atomic.
pub(super) struct CommitPanel {
    pub(super) message: String,
    pub(super) amend: bool,
    /// The one write holding the index, if any.
    write: Option<Running>,
    /// The one push in flight, if any: takes no lock, so it runs beside a
    /// write. Dropping it cancels (kills) the push.
    push: Option<Job<Report>>,
    ai: Option<Job<Result<String, String>>>,
    /// Amend was ticked over a blank message: the last one, loading.
    last: Option<Job<Result<String, String>>>,
    notice: Option<Report>,
}
impl CommitPanel {
    pub(super) fn new() -> Self {
        Self {
            message: String::new(),
            amend: false,
            write: None,
            push: None,
            ai: None,
            last: None,
            notice: None,
        }
    }
    /// A write holds the index: no other write, and no commit, until it lands.
    pub(super) fn busy(&self) -> bool {
        self.write.is_some()
    }
    /// A push is in flight: no second push until it lands or is cancelled.
    pub(super) fn pushing(&self) -> bool {
        self.push.is_some()
    }
    /// Start a write unless its slot is taken. Returns whether it started.
    pub(super) fn start(&mut self, ctx: &egui::Context, cwd: &Path, write: Write) -> bool {
        let is_push = matches!(write, Write::Push(_));
        let taken = if is_push { self.pushing() } else { self.busy() };
        if taken || write.is_empty() {
            return false;
        }
        let cwd = cwd.to_path_buf();
        self.notice = None;
        let doing = write.doing();
        let job = Job::start(ctx, move |cancel| run(&cwd, &write, cancel));
        if is_push {
            self.push = Some(job);
        } else {
            self.write = Some(Running { job, doing });
        }
        true
    }
    /// Kill a running push. Its reply never arrives (the job is gone), so
    /// the notice is written here. Returns whether there was one to kill;
    /// then the status wants a re-read, since the remote may have taken
    /// the push before the kill landed.
    fn cancel_push(&mut self) -> bool {
        let was = self.push.take().is_some();
        if was {
            self.notice = Some(Report::new(Ok(format!("Push cancelled. {}", push::RETRY))));
        }
        was
    }
    fn start_draft(&mut self, ctx: &egui::Context, cwd: PathBuf, paths: Option<Vec<String>>) {
        let settings = crate::config::live(ctx);
        let (provider, model) = (settings.title_provider, settings.title_model.clone());
        self.notice = None;
        self.ai = Some(Job::start(ctx, move |cancel| {
            draft(&cwd, paths.as_deref(), provider, &model, cancel)
        }));
    }
    fn poll(&mut self) -> Outcome {
        let mut outcome = Outcome::default();
        let stopped = || Report::new(Err("Git worker stopped".into()));
        if let Some(reply) = self.write.as_ref().and_then(|w| w.job.poll()) {
            self.write = None;
            outcome.wrote = true;
            let report = reply.unwrap_or_else(stopped);
            if report.committed {
                self.message.clear();
                self.amend = false;
            }
            outcome.push = report.push;
            self.notice = (!report.text.is_empty()).then_some(report);
        }
        // A push moves refs/remotes: re-read, whether or not the watch
        // also noticed.
        if let Some(reply) = self.push.as_ref().and_then(Job::poll) {
            self.push = None;
            outcome.wrote = true;
            self.notice = Some(reply.unwrap_or_else(stopped));
        }
        if let Some(reply) = self.ai.as_ref().and_then(Job::poll) {
            self.ai = None;
            match reply.unwrap_or_else(|| Err("AI worker stopped".into())) {
                Ok(message) => self.message = message,
                Err(text) => self.notice = Some(Report::new(Err(text))),
            }
        }
        if let Some(reply) = self.last.as_ref().and_then(Job::poll) {
            self.last = None;
            match reply.unwrap_or_else(|| Err("Git worker stopped".into())) {
                // Only into a box still blank: never over what was typed meanwhile.
                Ok(message) if self.amend && self.message.trim().is_empty() => {
                    self.message = message
                }
                Ok(_) => {}
                Err(text) => self.notice = Some(Report::new(Err(text))),
            }
        }
        outcome
    }

    /// Paint the panel bottom-up inside `rect`; returns the height it used
    /// (the tree takes the rest) and what happened.
    pub(super) fn show(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        cwd: Option<&Path>,
        scope: &Scope,
        base: egui::Id,
    ) -> (f32, Outcome) {
        let mut outcome = self.poll();
        let th = crate::theme::live(ui.ctx());
        let mut ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(base.with("commit"))
                .max_rect(rect)
                .layout(egui::Layout::bottom_up(egui::Align::Min)),
        );
        let mut commit = None;
        if let Some(notice) = &self.notice {
            let color = if notice.error { th.danger } else { th.dim };
            // A ScrollArea pins itself to the top of whatever space is
            // free, which in this bottom-up ui is the top of the window; the
            // rest of the panel would then land above it, off-screen, and
            // the tree would get no room. So measure the text and give the
            // area an exact rect at the bottom: at most five lines, scrolling.
            let font = egui::TextStyle::Body.resolve(ui.style());
            let width = ui.available_width();
            let galley = ui.fonts_mut(|f| f.layout(notice.text.clone(), font, color, width));
            let lines = ui.text_style_height(&egui::TextStyle::Body) * NOTICE_LINES;
            let height = galley.size().y.min(lines);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
            let mut area = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(rect)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            egui::ScrollArea::vertical()
                .id_salt("notice")
                .show(&mut area, |ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(&notice.text).color(color))
                            .wrap()
                            .selectable(true),
                    );
                });
        }
        let amend = self.amend && !scope.unborn && !scope.merging;
        let blank = self.message.trim().is_empty();
        let writing = self.write.is_some();
        let doing = self.write.as_ref().and_then(|w| w.doing);
        let pushing = self.push.is_some();
        // Amend with nothing picked rewrites just the message.
        let something = scope.files > 0 || amend;
        let ready = cwd.is_some()
            && !scope.conflicts
            && something
            && !blank
            && !writing
            && self.ai.is_none();
        let what = match (scope.files, scope.checked) {
            (0, _) => "the message only".to_owned(),
            (1, true) => "the 1 checked file".to_owned(),
            (n, true) => format!("the {n} checked files"),
            (1, false) => "the 1 changed file".to_owned(),
            (n, false) => format!("all {n} changed files"),
        };
        let verb = if amend {
            "Amend the last commit with"
        } else {
            "Commit"
        };
        let commit_label = if amend { "Amend Commit" } else { "Commit" };
        let push_label = if amend {
            "Amend Commit and Push…"
        } else {
            "Commit and Push…"
        };
        let provider = crate::config::live(ui.ctx()).title_provider;
        let can_draft = cwd.is_some() && scope.files > 0 && !writing;
        let draft_hint = format!(
            "Draft a message from the diff of {what} with {} (Settings: session-title provider). Replaces the text below.",
            ai_oneshot::program(provider)
        );
        // The full row, or just Commit and a "⋯" menu when it won't fit.
        let compact = {
            let font = egui::TextStyle::Button.resolve(ui.style());
            let pad = ui.spacing().button_padding.x * 2.0;
            let gap = ui.spacing().item_spacing.x;
            let text = |t: &str| {
                ui.painter()
                    .layout_no_wrap(t.to_owned(), font.clone(), egui::Color32::WHITE)
                    .size()
                    .x
            };
            let button = |t: &str| text(t) + pad + gap;
            let spinner = ui.spacing().interact_size.y + gap;
            let mut need = button(commit_label) + button(push_label);
            if let Some(doing) = doing {
                need += spinner + text(doing) + gap;
            }
            if pushing {
                need += spinner + text(PUSHING) + gap + button("Cancel");
            }
            need += if self.ai.is_some() {
                button("Cancel") + spinner + text("Writing message…")
            } else {
                button("AI message")
            };
            need > ui.available_width()
        };
        ui.horizontal(|ui| {
            let why = if scope.conflicts {
                "Resolve the conflicts first"
            } else if !something {
                "Nothing to commit"
            } else if blank {
                "Write a commit message"
            } else {
                "Busy"
            };
            let hint = |r: egui::Response, ok: &str| {
                if ready {
                    r.on_hover_text(ok)
                } else {
                    r.on_disabled_hover_text(why)
                }
            };
            let ok_commit = format!("{verb} {what} (Ctrl+Enter)");
            let ok_push = format!("{verb} {what}, then review the push");
            if hint(
                ui.add_enabled(ready, egui::Button::new(commit_label)),
                &ok_commit,
            )
            .clicked()
            {
                commit = Some(false);
            }
            if compact {
                if doing.is_some() || pushing || self.ai.is_some() {
                    ui.spinner();
                }
                let side = ui.spacing().interact_size.y;
                let more = toolbar::button(ui, Glyph::Dots, side, true, false, "More actions");
                let tool = |label: &str, hint: &str, enabled| Tool {
                    glyph: Glyph::Dots,
                    label: label.to_owned(),
                    hint: hint.to_owned(),
                    enabled,
                    on: false,
                };
                let ai = self.ai.is_some();
                let mut tools = vec![
                    tool(push_label, if ready { &ok_push } else { why }, ready),
                    if ai {
                        tool("Cancel", "Stop writing the message", true)
                    } else {
                        tool("AI message", &draft_hint, can_draft)
                    },
                ];
                if pushing {
                    tools.push(tool("Cancel push", CANCEL_PUSH, true));
                }
                match toolbar::overflow(&more, &tools) {
                    Some(0) => commit = Some(true),
                    Some(1) if ai => self.ai = None,
                    Some(1) => {
                        if let Some(cwd) = cwd {
                            let paths = scope.checked.then(|| scope.paths.clone());
                            self.start_draft(ui.ctx(), cwd.to_path_buf(), paths);
                        }
                    }
                    Some(_) => outcome.wrote |= self.cancel_push(),
                    None => {}
                }
                return;
            }
            if hint(
                ui.add_enabled(ready, egui::Button::new(push_label)),
                &ok_push,
            )
            .clicked()
            {
                commit = Some(true);
            }
            if let Some(doing) = doing {
                ui.spinner();
                ui.label(egui::RichText::new(doing).color(th.dim));
            }
            if pushing {
                ui.spinner();
                ui.label(egui::RichText::new(PUSHING).color(th.dim));
                if ui.button("Cancel").on_hover_text(CANCEL_PUSH).clicked() {
                    outcome.wrote |= self.cancel_push();
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.ai.is_some() {
                    if ui.button("Cancel").clicked() {
                        self.ai = None;
                    }
                    ui.spinner();
                    ui.label(egui::RichText::new("Writing message…").color(th.dim));
                } else {
                    let r = ui
                        .add_enabled(can_draft, egui::Button::new("AI message"))
                        .on_hover_text(&draft_hint)
                        .on_disabled_hover_text("Nothing to commit");
                    if r.clicked()
                        && let Some(cwd) = cwd
                    {
                        let paths = scope.checked.then(|| scope.paths.clone());
                        self.start_draft(ui.ctx(), cwd.to_path_buf(), paths);
                    }
                }
            });
        });
        let id = base.with("commit-message");
        // Ctrl+Enter commits. Taken before the TextEdit runs, which would
        // otherwise see the Enter first.
        if ui.memory(|m| m.has_focus(id))
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter))
            && ready
        {
            commit = Some(false);
        }
        let height = ui.text_style_height(&egui::TextStyle::Monospace) * 4.0 + 8.0;
        ui.add_sized(
            [ui.available_width(), height],
            egui::TextEdit::multiline(&mut self.message)
                .id(id)
                .hint_text("Commit message")
                .font(egui::TextStyle::Monospace)
                .interactive(!writing),
        );
        ui.horizontal(|ui| {
            let r = ui
                .add_enabled(
                    !scope.unborn && !scope.merging && !writing,
                    egui::Checkbox::new(&mut self.amend, "Amend"),
                )
                .on_hover_text("Rewrite the last commit instead of adding a new one")
                .on_disabled_hover_text(if scope.merging {
                    "Not while merging"
                } else {
                    "No commit to amend yet"
                });
            if r.changed()
                && self.amend
                && self.message.trim().is_empty()
                && let Some(cwd) = cwd
            {
                let cwd = cwd.to_path_buf();
                self.last = Some(Job::start(ui.ctx(), move |c| last_message(&cwd, c)));
            }
        });
        if let (Some(push), Some(cwd)) = (commit, cwd) {
            let write = Write::Commit {
                message: self.message.clone(),
                paths: scope.paths.clone(),
                amend,
                push,
                merge: scope.merging,
            };
            self.start(ui.ctx(), cwd, write);
        }
        (ui.min_rect().height(), outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_history::tests::git;
    use crate::job::DeathWatch;
    use std::time::Instant;

    #[test]
    fn ai_output_must_be_plain_non_empty_text() {
        assert_eq!(
            clean_message("  feat(x): add y\r\n\r\nWhy.\r\n").unwrap(),
            "feat(x): add y\n\nWhy."
        );
        assert!(clean_message(" \n ").is_err());
        assert!(clean_message("```\nfeat: x\n```").is_err());
        assert!(clean_message("feat: x\u{1b}[31m").is_err());
    }

    fn ok(report: Report) {
        assert!(!report.error, "{report:?}");
    }
    fn never() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }
    /// `super::run` with a flag nobody sets.
    fn run(cwd: &Path, write: &Write) -> Report {
        super::run(cwd, write, &never())
    }

    fn repo() -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        for args in [
            &["init", "-b", "main"][..],
            &["config", "user.name", "Commit Test"],
            &["config", "user.email", "commit@example.test"],
            &["config", "commit.gpgsign", "false"],
        ] {
            git(repo.path(), args);
        }
        repo
    }

    fn commit(dir: &Path, message: &str, paths: &[&str], amend: bool) -> Report {
        run(
            dir,
            &Write::Commit {
                message: message.into(),
                paths: paths.iter().map(|p| (*p).to_owned()).collect(),
                amend,
                push: false,
                merge: false,
            },
        )
    }
    fn strings(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|p| (*p).to_owned()).collect()
    }

    #[test]
    fn commit_only_takes_the_given_paths_and_leaves_the_rest_of_the_index() {
        let repo = repo();
        let dir = repo.path();
        // Pathspec magic characters are literal file names.
        for f in ["[a].txt", "a.txt", "b.txt"] {
            std::fs::write(dir.join(f), "one\n").unwrap();
        }
        // Add to VCS before the first commit: tracked, nothing staged.
        ok(run(
            dir,
            &Write::Track(strings(&["[a].txt", "a.txt", "b.txt"])),
        ));
        assert_eq!(git(dir, &["ls-files"]), "[a].txt\na.txt\nb.txt");
        let report = commit(dir, "feat: first\n\nBody line.", &["[a].txt"], false);
        assert!(report.committed && !report.error, "{report:?}");
        assert!(report.text.starts_with("[main (root-commit)"), "{report:?}");
        assert_eq!(git(dir, &["show", "--name-only", "--format="]), "[a].txt");
        assert_eq!(
            git(dir, &["log", "-1", "--format=%B"]),
            "feat: first\n\nBody line."
        );
        // An agent staged b.txt; committing a.txt leaves b.txt staged as it was.
        git(dir, &["add", "b.txt"]);
        std::fs::write(dir.join("a.txt"), "two\n").unwrap();
        ok(commit(dir, "feat: a", &["a.txt"], false));
        assert_eq!(git(dir, &["show", "--name-only", "--format="]), "a.txt");
        assert_eq!(git(dir, &["diff", "--cached", "--name-only"]), "b.txt");
        // The working copy is what's committed, staged or not.
        assert_eq!(git(dir, &["show", "HEAD:a.txt"]), "two");
        // Amend with no paths rewrites the message only.
        ok(commit(dir, "feat: a, amended", &[], true));
        assert_eq!(git(dir, &["log", "-1", "--format=%s"]), "feat: a, amended");
        assert_eq!(git(dir, &["rev-list", "--count", "HEAD"]), "2");
        assert_eq!(git(dir, &["diff", "--cached", "--name-only"]), "b.txt");
        // Nothing to commit: the failure carries Git's own words.
        let report = commit(dir, "x", &["a.txt"], false);
        assert!(report.error && !report.committed);
        assert!(report.text.starts_with("Commit failed:"), "{report:?}");
    }

    #[test]
    fn rollback_restores_versioned_files_and_untracks_new_ones() {
        let repo = repo();
        let dir = repo.path();
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        std::fs::write(dir.join("old.txt"), "old\n").unwrap();
        std::fs::write(dir.join("gone.txt"), "gone\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "base"]);
        std::fs::write(dir.join("a.txt"), "edited\n").unwrap();
        git(dir, &["add", "a.txt"]);
        std::fs::write(dir.join("a.txt"), "edited again\n").unwrap();
        std::fs::remove_file(dir.join("gone.txt")).unwrap();
        git(dir, &["mv", "old.txt", "new.txt"]);
        std::fs::write(dir.join("n.txt"), "new\n").unwrap();
        ok(run(dir, &Write::Track(strings(&["n.txt"]))));
        ok(run(
            dir,
            &Write::Rollback {
                untrack: strings(&["n.txt", "new.txt"]),
                restore: strings(&["a.txt", "gone.txt", "old.txt"]),
            },
        ));
        // Back to HEAD; new files kept on disk, unversioned.
        assert_eq!(
            git(dir, &["status", "--porcelain", "--untracked-files=all"]),
            "?? n.txt\n?? new.txt"
        );
        // Trimmed: core.autocrlf may check it out with CRLF.
        let a = std::fs::read_to_string(dir.join("a.txt")).unwrap();
        assert_eq!(a.trim_end(), "a");
        assert!(dir.join("gone.txt").exists());
    }

    #[test]
    fn a_merge_commits_the_resolved_index_not_a_partial_commit() {
        let repo = repo();
        let dir = repo.path();
        std::fs::write(dir.join("f.txt"), "base\n").unwrap();
        git(dir, &["add", "f.txt"]);
        git(dir, &["commit", "-m", "base"]);
        git(dir, &["checkout", "-q", "-b", "other"]);
        std::fs::write(dir.join("f.txt"), "other\n").unwrap();
        git(dir, &["commit", "-qam", "other"]);
        git(dir, &["checkout", "-q", "main"]);
        std::fs::write(dir.join("f.txt"), "main\n").unwrap();
        git(dir, &["commit", "-qam", "main"]);
        let merge = std::process::Command::new("git")
            .current_dir(dir)
            .args(["merge", "other"])
            .output()
            .unwrap();
        assert!(!merge.status.success(), "the merge must conflict");
        std::fs::write(dir.join("f.txt"), "resolved\n").unwrap();
        ok(run(dir, &Write::Resolve(strings(&["f.txt"]))));
        // Git refuses `commit --only <paths>` mid-merge.
        let partial = commit(dir, "merge", &["f.txt"], false);
        assert!(
            partial.error && partial.text.contains("partial commit"),
            "{partial:?}"
        );
        let report = run(
            dir,
            &Write::Commit {
                message: "Merge other".into(),
                paths: strings(&["f.txt"]),
                amend: false,
                push: false,
                merge: true,
            },
        );
        assert!(report.committed && !report.error, "{report:?}");
        let parents = git(dir, &["rev-list", "--parents", "-1", "HEAD"]);
        assert_eq!(parents.split(' ').count(), 3, "a merge commit");
        assert_eq!(git(dir, &["show", "HEAD:f.txt"]), "resolved");
    }

    #[test]
    fn ignore_appends_escaped_anchored_patterns() {
        assert_eq!(ignore_pattern("a/b.txt"), "/a/b.txt");
        assert_eq!(ignore_pattern("x[1]*?.log"), "/x\\[1]\\*\\?.log");
        assert_eq!(ignore_pattern("nested/"), "/nested/");
        assert_eq!(ignore_pattern("trail "), "/trail\\ ");
        let repo = repo();
        let dir = repo.path();
        std::fs::write(dir.join(".gitignore"), "target").unwrap();
        for f in ["x[1].log", "keep.txt"] {
            std::fs::write(dir.join(f), "x").unwrap();
        }
        ok(run(dir, &Write::Ignore(strings(&["x[1].log"]))));
        assert_eq!(
            std::fs::read_to_string(dir.join(".gitignore")).unwrap(),
            "target\n/x\\[1].log\n"
        );
        assert_eq!(
            git(dir, &["status", "--porcelain", "--untracked-files=all"]),
            "?? .gitignore\n?? keep.txt"
        );
        assert!(run(dir, &Write::Ignore(strings(&["../out"]))).error);
        assert!(run(dir, &Write::Delete(strings(&["../out"]))).error);
    }

    #[test]
    fn a_failing_hook_reports_its_output_and_keeps_the_index() {
        let repo = repo();
        let dir = repo.path();
        let hook = dir.join(".git/hooks/pre-commit");
        std::fs::write(&hook, "#!/bin/sh\necho 'lint says no' >&2\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        git(dir, &["add", "a.txt"]);
        let report = commit(dir, "x", &["a.txt"], false);
        assert!(report.error && !report.committed);
        assert!(report.text.contains("lint says no"), "{report:?}");
        assert_eq!(git(dir, &["diff", "--cached", "--name-only"]), "a.txt");
        assert!(!dir.join(".git/index.lock").exists());
    }

    #[test]
    fn push_publishes_a_new_branch_then_reports_a_rejection() {
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "--bare", "-b", "main"]);
        let url = remote.path().to_string_lossy().into_owned();
        let commit_and_push = |dir: &Path, file: &str| {
            std::fs::write(dir.join(file), file).unwrap();
            ok(run(dir, &Write::Track(vec![file.into()])));
            let report = run(
                dir,
                &Write::Commit {
                    message: format!("add {file}"),
                    paths: vec![file.into()],
                    amend: false,
                    push: true,
                    merge: false,
                },
            );
            assert!(report.committed && report.push, "{report:?}");
            let never = Arc::new(AtomicBool::new(false));
            run(dir, &Write::Push(push::target(dir, &never).unwrap()))
        };
        let one = repo();
        git(one.path(), &["remote", "add", "origin", &url]);
        // No upstream: publishes to origin/main and tracks it.
        let pushed = commit_and_push(one.path(), "one.txt");
        assert_eq!(pushed.text, "Pushed main → origin/main");
        ok(pushed);
        assert_eq!(
            git(one.path(), &["rev-parse", "--abbrev-ref", "@{u}"]),
            "origin/main"
        );
        let two = repo();
        git(two.path(), &["remote", "add", "origin", &url]);
        let rejected = commit_and_push(two.path(), "two.txt");
        assert!(rejected.error);
        assert!(rejected.text.starts_with("Push REJECTED"), "{rejected:?}");
        // The local commit still landed.
        assert_eq!(
            git(two.path(), &["log", "-1", "--format=%s"]),
            "add two.txt"
        );
    }

    /// A repository whose push hangs for good: `core.sshCommand` is a
    /// PowerShell sleeper, and the remote's ssh-style URL makes `git push`
    /// run it. Before sleeping it records its process chain up to the
    /// outermost `git.exe` (leaf first, `pid=name` each) in `chain`, so a
    /// test can watch every process the push spawned.
    struct HangingPush {
        repo: tempfile::TempDir,
        chain: PathBuf,
    }
    fn hanging_push() -> HangingPush {
        let repo = repo();
        let dir = repo.path();
        git(dir, &["commit", "--allow-empty", "-m", "one"]);
        git(dir, &["remote", "add", "origin", "nowhere:repo"]);
        let script = dir.join("sleeper.ps1");
        let chain = dir.join("chain.txt");
        // Git for Windows' `git.exe` on PATH is a wrapper that runs the real
        // one as a child: walk past a git.exe whose parent is one too.
        let body = format!(
            "$all = @{{}}\n\
             Get-CimInstance Win32_Process | ForEach-Object {{ $all[[int]$_.ProcessId] = $_ }}\n\
             $p = [int]$PID; $chain = @()\n\
             for ($i = 0; $i -lt 8 -and $all.ContainsKey($p); $i++) {{\n\
               $proc = $all[$p]; $chain += \"$p=$($proc.Name)\"\n\
               $parent = $all[[int]$proc.ParentProcessId]\n\
               if ($proc.Name -eq 'git.exe' -and $parent.Name -ne 'git.exe') {{ break }}\n\
               $p = [int]$proc.ParentProcessId\n\
             }}\n\
             Set-Content -LiteralPath '{}' -Value ($chain -join ',')\n\
             Start-Sleep 600\n",
            chain.display()
        );
        std::fs::write(&script, body).unwrap();
        // Through `sh -c` (the spaces see to that): forward slashes and quotes.
        let ssh = format!(
            "powershell -NoProfile -ExecutionPolicy Bypass -File \"{}\"",
            script.display().to_string().replace('\\', "/")
        );
        git(dir, &["config", "core.sshCommand", &ssh]);
        HangingPush { repo, chain }
    }
    /// The sleeper's tree once it has reported it, with a watch on each
    /// process opened while it is alive (so pid reuse can't fool the test).
    fn watch_tree(chain: &Path) -> Vec<(String, DeathWatch)> {
        let deadline = Instant::now() + Duration::from_secs(30);
        let tree = loop {
            let parsed: Option<Vec<(u32, String)>> =
                std::fs::read_to_string(chain).ok().and_then(|text| {
                    text.trim()
                        .split(',')
                        .map(|e| {
                            let (pid, name) = e.split_once('=')?;
                            Some((pid.parse().ok()?, name.to_owned()))
                        })
                        .collect()
                });
            if let Some(tree) = parsed
                && tree.last().is_some_and(|(_, name)| name == "git.exe")
            {
                break tree;
            }
            assert!(
                Instant::now() < deadline,
                "the sleeper never reported its tree"
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(tree.len() >= 2, "{tree:?}");
        tree.into_iter()
            .map(|(pid, name)| {
                let watch = DeathWatch::open(pid).unwrap_or_else(|| panic!("cannot watch {name}"));
                (format!("{name} ({pid})"), watch)
            })
            .collect()
    }
    fn assert_all_dead(tree: &[(String, DeathWatch)]) {
        for (who, watch) in tree {
            assert!(watch.dead_within_ms(5000), "{who} survived");
        }
    }

    #[test]
    fn a_cancelled_push_takes_its_whole_process_tree_with_it() {
        let hang = hanging_push();
        let dir = hang.repo.path().to_path_buf();
        let target = push::target(&dir, &never()).unwrap();
        let cancel = never();
        let flag = cancel.clone();
        let worker = std::thread::spawn(move || super::run(&dir, &Write::Push(target), &flag));
        let tree = watch_tree(&hang.chain);
        assert!(
            !tree[0].1.dead_within_ms(200),
            "the sleeper died on its own"
        );
        assert!(
            !worker.is_finished(),
            "the push returned without being cancelled"
        );
        cancel.store(true, Ordering::Relaxed);
        let report = worker.join().unwrap();
        assert!(
            report.error && report.text.starts_with("Push cancelled"),
            "{report:?}"
        );
        assert!(report.text.contains("already landed"), "{report:?}");
        assert_all_dead(&tree);
    }

    #[test]
    fn a_timed_out_push_takes_its_whole_process_tree_with_it() {
        let hang = hanging_push();
        let dir = hang.repo.path().to_path_buf();
        // Long enough for the sleeper to report its tree first.
        let timeout = Duration::from_secs(8);
        let worker = std::thread::spawn(move || {
            git::write(
                &dir,
                &["push", "origin", "refs/heads/main:refs/heads/main"],
                None,
                None,
                Some(timeout),
            )
        });
        let tree = watch_tree(&hang.chain);
        assert!(
            !worker.is_finished(),
            "the push returned before its timeout"
        );
        assert_eq!(worker.join().unwrap(), Err(GitError::TimedOut));
        assert_all_dead(&tree);
    }

    #[test]
    fn the_panel_commits_beside_a_running_push_and_refuses_a_second_push() {
        let hang = hanging_push();
        let dir = hang.repo.path();
        let ctx = egui::Context::default();
        let target = push::target(dir, &never()).unwrap();
        let mut panel = CommitPanel::new();
        assert!(panel.start(&ctx, dir, Write::Push(target.clone())));
        assert!(panel.pushing() && !panel.busy());
        assert!(
            !panel.start(&ctx, dir, Write::Push(target)),
            "one push at a time"
        );
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        git(dir, &["add", "-N", "a.txt"]);
        let commit = Write::Commit {
            message: "feat: a".into(),
            paths: strings(&["a.txt"]),
            amend: false,
            push: false,
            merge: false,
        };
        assert!(
            panel.start(&ctx, dir, commit),
            "a commit runs beside the push"
        );
        assert!(panel.busy() && panel.pushing());
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut outcome = Outcome::default();
        while panel.busy() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
            outcome = panel.poll();
        }
        assert!(!panel.busy() && outcome.wrote, "the commit did not land");
        assert_eq!(git(dir, &["log", "-1", "--format=%s"]), "feat: a");
        assert!(panel.pushing(), "the push is still in flight");
        // Closing the Changes window drops the panel: the push dies with it.
        let tree = watch_tree(&hang.chain);
        drop(panel);
        assert_all_dead(&tree);
    }

    /// One frame of the panel in a 400x600 window; the height it used.
    fn show_once(ctx: &egui::Context, panel: &mut CommitPanel) -> f32 {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 600.0));
        let mut used = 0.0;
        let _ = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(rect),
                ..Default::default()
            },
            |ui| {
                let scope = Scope::default();
                let base = egui::Id::new("commit-test");
                used = panel.show(ui, rect, None, &scope, base).0;
            },
        );
        used
    }

    /// A notice is a few lines at the bottom of the panel, not a scroll
    /// area pinned to the top of the window with the panel pushed off
    /// above it and no room left for the tree (seen after every commit).
    #[test]
    fn a_notice_adds_a_few_lines_to_the_panel_not_the_whole_window() {
        let ctx = egui::Context::default();
        let mut panel = CommitPanel::new();
        show_once(&ctx, &mut panel);
        let plain = show_once(&ctx, &mut panel);
        assert!(plain > 0.0 && plain < 300.0, "{plain}");
        let mut line = 0.0;
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            line = ui.text_style_height(&egui::TextStyle::Body);
        });
        panel.notice = Some(Report::new(Ok("[main 1a2b3c4] subject".into())));
        show_once(&ctx, &mut panel);
        let short = show_once(&ctx, &mut panel);
        assert!(
            short > plain && short < plain + line * 2.5,
            "one line: {plain} -> {short}"
        );
        panel.notice = Some(Report::new(Err("line\n".repeat(40))));
        show_once(&ctx, &mut panel);
        let long = show_once(&ctx, &mut panel);
        assert!(
            long > short && long < plain + line * (NOTICE_LINES + 1.5),
            "capped: {plain} -> {long}"
        );
    }

    #[test]
    fn draft_prompt_carries_the_diff_against_head_or_its_stat() {
        let repo = repo();
        let dir = repo.path();
        let never = Arc::new(AtomicBool::new(false));
        assert_eq!(
            draft_prompt(dir, None, &never),
            Err("Nothing to commit".into())
        );
        // No HEAD yet: an Add to VCS file (intent-to-add) is in the draft.
        std::fs::write(dir.join("small.txt"), "hello\n").unwrap();
        git(dir, &["add", "-N", "small.txt"]);
        let prompt = draft_prompt(dir, None, &never).unwrap();
        assert!(prompt.contains("+hello"), "{prompt}");
        git(dir, &["add", "small.txt"]);
        git(dir, &["commit", "-m", "style: sample subject"]);
        // Unstaged edits count; the checked paths narrow it.
        std::fs::write(dir.join("small.txt"), "changed\n").unwrap();
        std::fs::write(dir.join("other.txt"), "x\n").unwrap();
        git(dir, &["add", "-N", "other.txt"]);
        let prompt = draft_prompt(dir, Some(&["small.txt".into()]), &never).unwrap();
        assert!(
            prompt.contains("+changed") && !prompt.contains("other.txt"),
            "{prompt}"
        );
        std::fs::write(dir.join("big.txt"), "line\n".repeat(20_000)).unwrap();
        git(dir, &["add", "big.txt"]);
        let prompt = draft_prompt(dir, None, &never).unwrap();
        assert!(prompt.contains("style: sample subject"));
        assert!(prompt.contains("too large"));
        assert!(prompt.contains("big.txt"));
        assert!(prompt.len() < DIFF_CAP + 1024);
    }
}
