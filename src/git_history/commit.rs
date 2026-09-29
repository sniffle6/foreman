//! The Git Changes window's writes: stage / unstage one file, and the commit
//! panel pinned under the tree (message box, Commit, Commit and Push, and a
//! one-click AI draft of the message). Every write runs on a worker through
//! `git::write`; one at a time, so two never race for `index.lock`.
use super::git::{self, GitError};
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

/// Staged diff sent to the AI; over this, only `--stat` goes.
const DIFF_CAP: usize = 32 * 1024;
const AI_TIMEOUT: Duration = Duration::from_secs(90);
const AI_MAX_OUTPUT: usize = 16 * 1024;
/// Killing a push is harmless (no local lock), unlike a commit.
const PUSH_TIMEOUT: Duration = Duration::from_secs(300);
const READ_TIMEOUT: Duration = Duration::from_secs(30);

const AI_RULES: &str = "You write Git commit messages. Reply with ONLY the commit \
message as plain text: a subject line of at most 72 characters, then optionally a \
blank line and a short body wrapped at 72 columns explaining why. Match the style \
of the recent commits shown (for example a `type(scope): subject` prefix). No code \
fences, no quotes around the message, no commentary before or after it.";

/// What a write does. Paths are repository-relative, as `git status` gave them.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Write {
    Stage(Vec<String>),
    Unstage(Vec<String>),
    Commit { message: String, push: bool },
}

/// A finished write, for the notice area.
#[derive(Debug, PartialEq)]
struct Report {
    text: String,
    error: bool,
    /// A commit landed (even if its push then failed): clear the message.
    committed: bool,
}

/// Worker-only: run one write.
fn run(cwd: &Path, write: &Write) -> Report {
    let report = |result: Result<String, String>, committed| match result {
        Ok(text) => Report {
            text,
            error: false,
            committed,
        },
        Err(text) => Report {
            text,
            error: true,
            committed,
        },
    };
    let with_paths = |head: &[&str], files: &[String]| -> Vec<String> {
        head.iter()
            .map(|a| (*a).to_owned())
            .chain(files.iter().cloned())
            .collect()
    };
    match write {
        Write::Stage(files) => {
            let args = with_paths(&["--literal-pathspecs", "add", "--"], files);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let result = git::write(cwd, &args, None, None).map(|_| String::new());
            report(result.map_err(|e| failure("Cannot stage", e)), false)
        }
        Write::Unstage(files) => {
            // `reset -- <paths>` rather than `restore --staged`: it also
            // works before the first commit, where there is no HEAD.
            let args = with_paths(&["--literal-pathspecs", "reset", "-q", "--"], files);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let result = git::write(cwd, &args, None, None).map(|_| String::new());
            report(result.map_err(|e| failure("Cannot unstage", e)), false)
        }
        Write::Commit { message, push } => {
            let out = match git::write(cwd, &["commit", "-F", "-"], Some(message), None) {
                Ok(out) => out,
                Err(e) => return report(Err(failure("Commit failed", e)), false),
            };
            // "[main 1a2b3c4] subject"; hooks may print lines before it.
            let summary = out
                .lines()
                .find(|l| l.starts_with('['))
                .unwrap_or("Committed")
                .to_owned();
            if !push {
                return report(Ok(summary), true);
            }
            match self::push(cwd) {
                Ok(()) => report(Ok(format!("{summary} — pushed")), true),
                // The commit stands; say so, so nobody commits it twice.
                Err(e) => report(Err(format!("{summary} — committed, but {e}")), true),
            }
        }
    }
}

fn failure(what: &str, error: GitError) -> String {
    match error {
        GitError::Failed(text) if text.is_empty() => format!("{what}: Git exited with an error"),
        GitError::Failed(text) => format!("{what}:\n{text}"),
        other => format!("{what}: {other}"),
    }
}

/// Push the current branch; with no upstream, publish it to `origin` and
/// track it there.
fn push(cwd: &Path) -> Result<(), String> {
    let never = Arc::new(AtomicBool::new(false));
    let upstream = git::output(
        cwd,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
        &never,
        4096,
        READ_TIMEOUT,
    )
    .is_ok();
    let args: &[&str] = if upstream {
        &["push"]
    } else {
        &["push", "-u", "origin", "HEAD"]
    };
    match git::write(cwd, args, None, Some(PUSH_TIMEOUT)) {
        Ok(_) => Ok(()),
        Err(GitError::Failed(text)) if rejected(&text) => Err(format!(
            "the push was REJECTED: the remote has commits this branch does not. \
             Pull (or rebase), then push again.\n{text}"
        )),
        Err(GitError::TimedOut) => Err("the push timed out after 5 minutes".into()),
        Err(e) => Err(failure("the push failed", e)),
    }
}

fn rejected(text: &str) -> bool {
    text.contains("[rejected]") || text.contains("non-fast-forward") || text.contains("fetch first")
}

/// Worker-only: the prompt for an AI draft, from the staged diff (or its
/// `--stat` when too large) and recent subjects for style.
fn draft_prompt(cwd: &Path, cancel: &Arc<AtomicBool>) -> Result<String, String> {
    let read = |args: &[&str], cap| git::output(cwd, args, cancel, cap, READ_TIMEOUT);
    let diff = match read(
        &["diff", "--cached", "--no-color", "--no-ext-diff", "-M"],
        DIFF_CAP,
    ) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(GitError::TooLarge) => {
            let stat = read(
                &["diff", "--cached", "--no-color", "--stat=200", "-M"],
                4 << 20,
            )
            .map_err(|e| format!("Cannot read the staged changes: {e}"))?;
            let mut stat = String::from_utf8_lossy(&stat).into_owned();
            truncate(&mut stat, DIFF_CAP);
            format!("(The full diff is too large; this is its summary.)\n{stat}")
        }
        Err(e) => return Err(format!("Cannot read the staged changes: {e}")),
    };
    if diff.trim().is_empty() {
        return Err("Nothing is staged".into());
    }
    // A repository with no commits yet has no log; that's fine.
    let log = read(&["log", "--oneline", "--no-color", "-10"], 64 * 1024)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    Ok(format!(
        "Recent commits, for style:\n<recent_commits>\n{log}</recent_commits>\n\n\
         Staged changes to describe:\n<staged_diff>\n{diff}\n</staged_diff>"
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
    provider: NamingProvider,
    model: &str,
    cancel: &Arc<AtomicBool>,
) -> Result<String, String> {
    let prompt = draft_prompt(cwd, cancel)?;
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

/// A worker's single reply; dropping it cancels (AI) or just stops
/// listening (writes, which always run to completion).
struct Job<T> {
    receiver: mpsc::Receiver<T>,
    cancel: Arc<AtomicBool>,
}
impl<T: Send + 'static> Job<T> {
    fn start(
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
    fn poll(&self) -> Option<Option<T>> {
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

struct Notice {
    text: String,
    error: bool,
}

/// What the panel did this frame, for the Changes window to act on.
#[derive(Default)]
pub(super) struct Outcome {
    /// A write finished: re-read the status (unless the watch will).
    pub(super) wrote: bool,
}

pub(super) struct CommitPanel {
    pub(super) message: String,
    write: Option<Job<Report>>,
    /// While a commit runs: whether it also pushes.
    committing: Option<bool>,
    ai: Option<Job<Result<String, String>>>,
    notice: Option<Notice>,
}
impl Drop for CommitPanel {
    fn drop(&mut self) {
        // A running write finishes on its own; don't wait for it here.
        let write = self.write.take();
        if write.is_some() {
            std::thread::spawn(move || drop(write));
        }
    }
}
impl CommitPanel {
    pub(super) fn new() -> Self {
        Self {
            message: String::new(),
            write: None,
            committing: None,
            ai: None,
            notice: None,
        }
    }
    pub(super) fn busy(&self) -> bool {
        self.write.is_some()
    }
    /// Start a write unless one is running. Returns whether it started.
    pub(super) fn start(&mut self, ctx: &egui::Context, cwd: &Path, write: Write) -> bool {
        if self.busy() {
            return false;
        }
        let cwd = cwd.to_path_buf();
        if let Write::Commit { push, .. } = write {
            self.notice = None;
            self.committing = Some(push);
        }
        self.write = Some(Job::start(ctx, move |_| run(&cwd, &write)));
        true
    }
    fn start_draft(&mut self, ctx: &egui::Context, cwd: PathBuf) {
        let settings = crate::config::live(ctx);
        let (provider, model) = (settings.title_provider, settings.title_model.clone());
        self.notice = None;
        self.ai = Some(Job::start(ctx, move |cancel| {
            draft(&cwd, provider, &model, cancel)
        }));
    }
    fn poll(&mut self) -> Outcome {
        let mut outcome = Outcome::default();
        if let Some(reply) = self.write.as_ref().and_then(Job::poll) {
            self.write = None;
            self.committing = None;
            outcome.wrote = true;
            let report = reply.unwrap_or(Report {
                text: "Git worker stopped".into(),
                error: true,
                committed: false,
            });
            if report.committed {
                self.message.clear();
            }
            self.notice = (!report.text.is_empty()).then_some(Notice {
                text: report.text,
                error: report.error,
            });
        }
        if let Some(reply) = self.ai.as_ref().and_then(Job::poll) {
            self.ai = None;
            match reply.unwrap_or_else(|| Err("AI worker stopped".into())) {
                Ok(message) => self.message = message,
                Err(text) => self.notice = Some(Notice { text, error: true }),
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
        staged: usize,
        base: egui::Id,
    ) -> (f32, Outcome) {
        let outcome = self.poll();
        let th = crate::theme::live(ui.ctx());
        let mut ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(base.with("commit"))
                .max_rect(rect)
                .layout(egui::Layout::bottom_up(egui::Align::Min)),
        );
        let mut commit = None;
        if let Some(notice) = &self.notice {
            egui::ScrollArea::vertical()
                .id_salt("notice")
                .max_height(ui.text_style_height(&egui::TextStyle::Body) * 5.0)
                .stick_to_bottom(false)
                .show(&mut ui, |ui| {
                    let color = if notice.error { th.danger } else { th.dim };
                    ui.add(
                        egui::Label::new(egui::RichText::new(&notice.text).color(color))
                            .wrap()
                            .selectable(true),
                    );
                });
        }
        let blank = self.message.trim().is_empty();
        let writing = self.write.is_some();
        ui.horizontal(|ui| {
            let ready = cwd.is_some() && staged > 0 && !blank && !writing && self.ai.is_none();
            let why = if staged == 0 {
                "Nothing is staged — stage files first"
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
            let staged_files = if staged == 1 {
                "the 1 staged file".to_owned()
            } else {
                format!("the {staged} staged files")
            };
            if hint(
                ui.add_enabled(ready, egui::Button::new("Commit")),
                &format!("Commit {staged_files} (Ctrl+Enter)"),
            )
            .clicked()
            {
                commit = Some(false);
            }
            if hint(
                ui.add_enabled(ready, egui::Button::new("Commit and Push")),
                &format!("Commit {staged_files}, then push the branch"),
            )
            .clicked()
            {
                commit = Some(true);
            }
            if let Some(push) = self.committing {
                ui.spinner();
                let doing = if push { "Committing and pushing…" } else { "Committing…" };
                ui.label(egui::RichText::new(doing).color(th.dim));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.ai.is_some() {
                    if ui.button("Cancel").clicked() {
                        self.ai = None;
                    }
                    ui.spinner();
                    ui.label(egui::RichText::new("Writing message…").color(th.dim));
                } else {
                    let provider = crate::config::live(ui.ctx()).title_provider;
                    let can = cwd.is_some() && staged > 0 && !writing;
                    let r = ui
                        .add_enabled(can, egui::Button::new("AI message"))
                        .on_hover_text(format!(
                            "Draft a message from the staged diff with {} (Settings: session-title provider). Replaces the text below.",
                            ai_oneshot::program(provider)
                        ))
                        .on_disabled_hover_text("Stage files first");
                    if r.clicked()
                        && let Some(cwd) = cwd
                    {
                        self.start_draft(ui.ctx(), cwd.to_path_buf());
                    }
                }
            });
        });
        let id = base.with("commit-message");
        // Ctrl+Enter commits. Taken before the TextEdit runs, which would
        // otherwise see the Enter first.
        if ui.memory(|m| m.has_focus(id))
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter))
            && staged > 0
            && !blank
            && !writing
            && self.ai.is_none()
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
        if let (Some(push), Some(cwd)) = (commit, cwd) {
            let message = self.message.clone();
            self.start(ui.ctx(), cwd, Write::Commit { message, push });
        }
        (ui.min_rect().height(), outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_history::tests::git;

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

    #[test]
    fn rejected_push_is_recognised() {
        assert!(rejected(" ! [rejected]        main -> main (fetch first)"));
        assert!(rejected(
            "error: failed to push some refs\nhint: ... non-fast-forward"
        ));
        assert!(!rejected(
            "fatal: 'origin' does not appear to be a git repository"
        ));
    }

    fn ok(report: Report) {
        assert!(!report.error, "{report:?}");
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

    #[test]
    fn stage_unstage_and_commit_before_and_after_the_first_commit() {
        let repo = repo();
        let dir = repo.path();
        // Pathspec magic characters are literal file names.
        std::fs::write(dir.join("[a].txt"), "a\n").unwrap();
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        let staged = || git(dir, &["diff", "--cached", "--name-only"]);
        ok(run(dir, &Write::Stage(vec!["[a].txt".into()])));
        assert_eq!(staged(), "[a].txt");
        // No HEAD yet: unstaging still works.
        ok(run(dir, &Write::Unstage(vec!["[a].txt".into()])));
        assert_eq!(staged(), "");
        ok(run(dir, &Write::Stage(vec!["a.txt".into()])));
        let message = "feat: first\n\nBody line.".to_owned();
        let report = run(
            dir,
            &Write::Commit {
                message,
                push: false,
            },
        );
        assert!(report.committed && !report.error);
        assert!(report.text.starts_with("[main (root-commit)"), "{report:?}");
        assert_eq!(
            git(dir, &["log", "-1", "--format=%B"]),
            "feat: first\n\nBody line."
        );
        // Born HEAD: unstage restores the committed version.
        std::fs::write(dir.join("a.txt"), "b\n").unwrap();
        ok(run(dir, &Write::Stage(vec!["a.txt".into()])));
        ok(run(dir, &Write::Unstage(vec!["a.txt".into()])));
        assert_eq!(staged(), "");
        // Nothing staged: the failure carries Git's own words.
        let report = run(
            dir,
            &Write::Commit {
                message: "x".into(),
                push: false,
            },
        );
        assert!(report.error && !report.committed);
        assert!(report.text.starts_with("Commit failed:"), "{report:?}");
    }

    #[test]
    fn a_failing_hook_reports_its_output_and_keeps_the_index() {
        let repo = repo();
        let dir = repo.path();
        let hooks = dir.join(".git/hooks");
        let hook = hooks.join("pre-commit");
        std::fs::write(&hook, "#!/bin/sh\necho 'lint says no' >&2\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        ok(run(dir, &Write::Stage(vec!["a.txt".into()])));
        let report = run(
            dir,
            &Write::Commit {
                message: "x".into(),
                push: false,
            },
        );
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
        let push_commit = |dir: &Path, file: &str| {
            std::fs::write(dir.join(file), file).unwrap();
            ok(run(dir, &Write::Stage(vec![file.into()])));
            run(
                dir,
                &Write::Commit {
                    message: format!("add {file}"),
                    push: true,
                },
            )
        };
        let one = repo();
        git(one.path(), &["remote", "add", "origin", &url]);
        // No upstream: push -u origin HEAD publishes and tracks.
        let pushed = push_commit(one.path(), "one.txt");
        assert!(
            !pushed.error && pushed.text.ends_with("— pushed"),
            "{pushed:?}"
        );
        assert_eq!(
            git(one.path(), &["rev-parse", "--abbrev-ref", "@{u}"]),
            "origin/main"
        );
        let two = repo();
        git(two.path(), &["remote", "add", "origin", &url]);
        let rejected = push_commit(two.path(), "two.txt");
        assert!(rejected.error && rejected.committed);
        assert!(
            rejected
                .text
                .contains("committed, but the push was REJECTED"),
            "{rejected:?}"
        );
        // The local commit still landed.
        assert_eq!(
            git(two.path(), &["log", "-1", "--format=%s"]),
            "add two.txt"
        );
    }

    #[test]
    fn draft_prompt_carries_the_staged_diff_or_its_stat() {
        let repo = repo();
        let dir = repo.path();
        let never = Arc::new(AtomicBool::new(false));
        assert_eq!(draft_prompt(dir, &never), Err("Nothing is staged".into()));
        std::fs::write(dir.join("small.txt"), "hello\n").unwrap();
        git(dir, &["add", "small.txt"]);
        let prompt = draft_prompt(dir, &never).unwrap();
        assert!(prompt.contains("+hello"), "{prompt}");
        git(dir, &["commit", "-m", "style: sample subject"]);
        std::fs::write(dir.join("big.txt"), "line\n".repeat(20_000)).unwrap();
        git(dir, &["add", "big.txt"]);
        let prompt = draft_prompt(dir, &never).unwrap();
        assert!(prompt.contains("style: sample subject"));
        assert!(prompt.contains("too large"));
        assert!(prompt.contains("big.txt"));
        assert!(prompt.len() < DIFF_CAP + 1024);
    }
}
