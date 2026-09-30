//! The push dialog, JetBrains style: after Commit and Push… (or Push…), show
//! where the branch goes and the commits that would leave, then Push or
//! Cancel. The read runs on a worker; the push itself is a `commit::Write`.
use super::commit::{Job, READ_TIMEOUT};
use super::git;
use eframe::egui;
use std::path::Path;
use std::sync::{Arc, atomic::AtomicBool};

/// Commits listed; the rest are counted as "more".
const LISTED: usize = 200;

#[derive(Debug, PartialEq)]
pub(super) struct Outgoing {
    pub(super) branch: String,
    /// The remote branch, as `origin/main`.
    pub(super) target: String,
    /// The target doesn't exist yet: the push publishes the branch.
    pub(super) new: bool,
    /// (short hash, subject), newest first.
    pub(super) commits: Vec<(String, String)>,
    /// More commits than listed.
    pub(super) more: bool,
}

/// Worker-only: what a push from `cwd` would send. The same choice as
/// `commit::push`: the upstream if there is one, else `origin/<branch>`.
pub(super) fn outgoing(cwd: &Path, cancel: &Arc<AtomicBool>) -> Result<Outgoing, String> {
    let read = |args: &[&str]| {
        git::output(cwd, args, cancel, 1 << 20, READ_TIMEOUT)
            .map(|b| String::from_utf8_lossy(&b).trim().to_owned())
    };
    let branch = read(&["symbolic-ref", "--short", "-q", "HEAD"])
        .map_err(|_| "Detached HEAD: check out a branch to push".to_owned())?;
    let (target, new, range): (String, bool, Vec<String>) =
        match read(&["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"]) {
            Ok(upstream) => (upstream, false, vec!["@{u}..HEAD".into()]),
            Err(_) => {
                read(&["remote", "get-url", "origin"]).map_err(|_| {
                    "This branch has no upstream and there is no remote named origin".to_owned()
                })?;
                let known = format!("refs/remotes/origin/{branch}");
                let new = read(&["rev-parse", "--verify", "-q", &known]).is_err();
                let range = vec!["HEAD".into(), "--not".into(), "--remotes=origin".into()];
                (format!("origin/{branch}"), new, range)
            }
        };
    let count = format!("-n{}", LISTED + 1);
    let mut args = vec!["log", "--no-color", "--format=%h%x1f%s", &count];
    args.extend(range.iter().map(String::as_str));
    let log = read(&args).map_err(|e| format!("Cannot read the outgoing commits: {e}"))?;
    let mut commits: Vec<(String, String)> = log
        .lines()
        .filter_map(|l| l.split_once('\u{1f}'))
        .map(|(h, s)| (h.to_owned(), s.to_owned()))
        .collect();
    let more = commits.len() > LISTED;
    commits.truncate(LISTED);
    Ok(Outgoing {
        branch,
        target,
        new,
        commits,
        more,
    })
}

pub(super) enum Choice {
    Pending,
    Cancel,
    Push,
}

pub(super) struct PushDialog {
    load: Option<Job<Result<Outgoing, String>>>,
    outgoing: Option<Result<Outgoing, String>>,
}
impl PushDialog {
    pub(super) fn open(ctx: &egui::Context, cwd: &Path) -> Self {
        let cwd = cwd.to_path_buf();
        Self {
            load: Some(Job::start(ctx, move |cancel| outgoing(&cwd, cancel))),
            outgoing: None,
        }
    }
    /// Whether Push would send anything: commits, or a branch to publish.
    fn pushable(&self) -> bool {
        matches!(&self.outgoing, Some(Ok(o)) if o.new || !o.commits.is_empty())
    }
    /// One frame of the modal. Esc or a click outside cancels; Enter pushes.
    pub(super) fn show(&mut self, ctx: &egui::Context, id: egui::Id) -> Choice {
        if let Some(reply) = self.load.as_ref().and_then(Job::poll) {
            self.load = None;
            self.outgoing = Some(reply.unwrap_or_else(|| Err("Git worker stopped".into())));
        }
        let th = crate::theme::live(ctx);
        let pushable = self.pushable();
        let mut choice = Choice::Pending;
        if pushable && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
            choice = Choice::Push;
        }
        let modal = egui::Modal::new(id).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.strong("Push Commits");
            ui.add_space(6.0);
            match &self.outgoing {
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(egui::RichText::new("Reading outgoing commits…").color(th.dim));
                    });
                }
                Some(Err(e)) => {
                    ui.colored_label(th.danger, e.as_str());
                }
                Some(Ok(o)) => {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&o.branch).color(th.text).strong());
                        ui.label(egui::RichText::new("→").color(th.dim));
                        ui.label(egui::RichText::new(&o.target).color(th.text).strong());
                        if o.new {
                            ui.label(egui::RichText::new("New").color(th.caret))
                                .on_hover_text("The branch isn't on the remote yet: the push publishes it and tracks it there");
                        }
                    });
                    ui.add_space(4.0);
                    if o.commits.is_empty() {
                        let text = if o.new {
                            "No new commits: the push only publishes the branch."
                        } else {
                            "Nothing to push: the remote branch is up to date."
                        };
                        ui.label(egui::RichText::new(text).color(th.dim));
                    } else {
                        egui::ScrollArea::vertical()
                            .max_height(260.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for (hash, subject) in &o.commits {
                                    ui.horizontal(|ui| {
                                        ui.label(egui::RichText::new(hash).color(th.dim).monospace());
                                        ui.add(
                                            egui::Label::new(egui::RichText::new(subject).color(th.text))
                                                .truncate(),
                                        );
                                    });
                                }
                                if o.more {
                                    ui.label(
                                        egui::RichText::new(format!("… and more (only {LISTED} listed)"))
                                            .color(th.dim),
                                    );
                                }
                            });
                    }
                }
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(pushable, egui::Button::new("Push")).clicked() {
                    choice = Choice::Push;
                }
                if ui.button("Cancel").clicked() {
                    choice = Choice::Cancel;
                }
            });
        });
        if matches!(choice, Choice::Pending) && modal.should_close() {
            choice = Choice::Cancel;
        }
        choice
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_history::tests::git;

    #[test]
    fn outgoing_lists_unpushed_commits_and_a_new_branch_target() {
        let never = Arc::new(AtomicBool::new(false));
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "--bare", "-b", "main"]);
        let repo = tempfile::tempdir().unwrap();
        let dir = repo.path();
        for args in [
            &["init", "-b", "main"][..],
            &["config", "user.name", "Push Test"],
            &["config", "user.email", "push@example.test"],
            &["config", "commit.gpgsign", "false"],
        ] {
            git(dir, args);
        }
        git(dir, &["commit", "--allow-empty", "-m", "one"]);
        assert!(
            outgoing(dir, &never)
                .unwrap_err()
                .contains("no remote named origin")
        );
        let url = remote.path().to_string_lossy().into_owned();
        git(dir, &["remote", "add", "origin", &url]);
        let first = outgoing(dir, &never).unwrap();
        assert_eq!(
            (first.branch.as_str(), first.target.as_str(), first.new),
            ("main", "origin/main", true)
        );
        assert_eq!(first.commits.len(), 1);
        assert_eq!(first.commits[0].1, "one");
        git(dir, &["push", "-u", "origin", "HEAD"]);
        git(dir, &["commit", "--allow-empty", "-m", "two"]);
        let tracked = outgoing(dir, &never).unwrap();
        assert!(!tracked.new);
        let subjects: Vec<_> = tracked.commits.iter().map(|c| c.1.as_str()).collect();
        assert_eq!(subjects, ["two"]);
        git(dir, &["checkout", "-q", "--detach"]);
        assert!(
            outgoing(dir, &never)
                .unwrap_err()
                .starts_with("Detached HEAD")
        );
    }
}
