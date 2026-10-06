//! Pushing, JetBrains style: after Commit and Push… (or Push…), a dialog
//! shows where the branch goes and the commits that would leave, then Push
//! or Cancel. One `Target` decides where: the dialog lists what goes there,
//! and the push (a `commit::Write::Push` carrying that same Target) names
//! both refs outright, so the two can never disagree.
use super::commit::{Job, READ_TIMEOUT, failure};
use super::git::{self, GitError};
use eframe::egui;
use std::path::Path;
use std::sync::{Arc, atomic::AtomicBool};
use std::time::Duration;

/// Commits listed; the rest are counted as "more".
const LISTED: usize = 200;
/// Killing a push is harmless (no local lock), unlike a commit.
const PUSH_TIMEOUT: Duration = Duration::from_secs(300);

/// Where a push of the current branch goes: its upstream when it has one on
/// a remote, else `origin/<branch>`, which the push then sets as upstream.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Target {
    /// The local branch.
    pub(super) branch: String,
    pub(super) remote: String,
    /// The branch's name on the remote.
    pub(super) remote_branch: String,
    /// The remote-tracking ref that mirrors it (`refs/remotes/origin/main`).
    tracking: String,
    /// Already the branch's upstream; otherwise the push sets it (`-u`).
    pub(super) tracked: bool,
    /// The remote branch doesn't exist yet: the push publishes it.
    pub(super) new: bool,
}
impl Target {
    /// `origin/main`.
    pub(super) fn label(&self) -> String {
        format!("{}/{}", self.remote, self.remote_branch)
    }
    /// Both refs named, so neither `push.default` nor a checkout between the
    /// dialog and the push can send it anywhere else.
    fn push_args(&self) -> Vec<String> {
        let mut args = vec!["push".to_owned()];
        if !self.tracked {
            args.push("-u".into());
        }
        args.push(self.remote.clone());
        args.push(format!(
            "refs/heads/{}:refs/heads/{}",
            self.branch, self.remote_branch
        ));
        args
    }
    /// `git log` revisions for the commits the push would send. A new
    /// branch lists what no branch on that remote has yet.
    fn outgoing_range(&self) -> Vec<String> {
        let local = format!("refs/heads/{}", self.branch);
        if self.new {
            vec![local, "--not".into(), format!("--remotes={}", self.remote)]
        } else {
            vec![format!("{}..{local}", self.tracking)]
        }
    }
}

/// Worker-only: where a push from `cwd` goes.
pub(super) fn target(cwd: &Path, cancel: &Arc<AtomicBool>) -> Result<Target, String> {
    let read = |args: &[&str]| {
        git::output(cwd, args, cancel, 64 * 1024, READ_TIMEOUT)
            .map(|b| String::from_utf8_lossy(&b).trim().to_owned())
    };
    let branch = read(&["symbolic-ref", "--short", "-q", "HEAD"])
        .map_err(|_| "Detached HEAD: check out a branch to push".to_owned())?;
    let local = format!("refs/heads/{branch}");
    let upstream = read(&[
        "for-each-ref",
        "--format=%(upstream:remotename)%00%(upstream:remoteref)%00%(upstream)",
        &local,
    ])
    .map_err(|e| format!("Cannot read the upstream: {e}"))?;
    let fields: Vec<&str> = upstream.split('\0').collect();
    let (remote, remote_branch, tracking, tracked) = match fields[..] {
        [".", ..] => {
            return Err(
                "This branch tracks a local branch: set an upstream on a remote to push".into(),
            );
        }
        [remote, merge, tracking] if !remote.is_empty() => {
            let remote_branch = merge
                .strip_prefix("refs/heads/")
                .ok_or_else(|| format!("Cannot push to the upstream {merge}"))?;
            let names = (remote.to_owned(), remote_branch.to_owned());
            (names.0, names.1, tracking.to_owned(), true)
        }
        _ => {
            read(&["remote", "get-url", "origin"]).map_err(|_| {
                "This branch has no upstream and there is no remote named origin".to_owned()
            })?;
            let tracking = format!("refs/remotes/origin/{branch}");
            ("origin".to_owned(), branch.clone(), tracking, false)
        }
    };
    let new = read(&["rev-parse", "--verify", "-q", &tracking]).is_err();
    Ok(Target {
        branch,
        remote,
        remote_branch,
        tracking,
        tracked,
        new,
    })
}

/// The notice for a push that was cancelled or timed out: the commit it
/// followed is in, only the push is missing.
pub(super) const RETRY: &str = "The commit already landed; push it again from Push…";

/// Worker-only: push `target`, exactly as the dialog showed it. `cancel`
/// kills the push and its whole process tree; so does `PUSH_TIMEOUT`.
pub(super) fn push(
    cwd: &Path,
    target: &Target,
    cancel: &Arc<AtomicBool>,
) -> Result<String, String> {
    let args = target.push_args();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match git::write(cwd, &args, None, Some(cancel), Some(PUSH_TIMEOUT)) {
        Ok(_) => Ok(format!("Pushed {} → {}", target.branch, target.label())),
        Err(GitError::Failed(text)) if rejected(&text) => Err(format!(
            "Push REJECTED: the remote has commits this branch does not. \
             Pull (or rebase), then push again.\n{text}"
        )),
        Err(GitError::TimedOut) => Err(format!(
            "The push timed out after {} minutes. {RETRY}",
            PUSH_TIMEOUT.as_secs() / 60
        )),
        Err(GitError::Cancelled) => Err(format!("Push cancelled. {RETRY}")),
        Err(e) => Err(failure("Push failed", e)),
    }
}

fn rejected(text: &str) -> bool {
    text.contains("[rejected]") || text.contains("non-fast-forward") || text.contains("fetch first")
}

#[derive(Debug, PartialEq)]
pub(super) struct Outgoing {
    pub(super) target: Target,
    /// (short hash, subject), newest first.
    pub(super) commits: Vec<(String, String)>,
    /// More commits than listed.
    pub(super) more: bool,
}

/// Worker-only: the target and the commits a push would send there.
pub(super) fn outgoing(cwd: &Path, cancel: &Arc<AtomicBool>) -> Result<Outgoing, String> {
    let target = target(cwd, cancel)?;
    let count = format!("-n{}", LISTED + 1);
    let range = target.outgoing_range();
    let mut args = vec!["log", "--no-color", "--format=%h%x1f%s", &count];
    args.extend(range.iter().map(String::as_str));
    let log = git::output(cwd, &args, cancel, 1 << 20, READ_TIMEOUT)
        .map_err(|e| format!("Cannot read the outgoing commits: {e}"))?;
    let mut commits: Vec<(String, String)> = String::from_utf8_lossy(&log)
        .lines()
        .filter_map(|l| l.split_once('\u{1f}'))
        .map(|(h, s)| (h.to_owned(), s.to_owned()))
        .collect();
    let more = commits.len() > LISTED;
    commits.truncate(LISTED);
    Ok(Outgoing {
        target,
        commits,
        more,
    })
}

pub(super) enum Choice {
    Pending,
    Cancel,
    /// Push to this target: the one the dialog showed.
    Push(Target),
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
    /// What Push sends to, if it would send anything: commits, or a
    /// branch to publish.
    fn pushable(&self) -> Option<Target> {
        match &self.outgoing {
            Some(Ok(o)) if o.target.new || !o.commits.is_empty() => Some(o.target.clone()),
            _ => None,
        }
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
        if let Some(target) = &pushable
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
        {
            choice = Choice::Push(target.clone());
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
                        let t = &o.target;
                        ui.label(egui::RichText::new(&t.branch).color(th.text).strong());
                        ui.label(egui::RichText::new("→").color(th.dim));
                        ui.label(egui::RichText::new(t.label()).color(th.text).strong());
                        if t.new {
                            ui.label(egui::RichText::new("New").color(th.caret))
                                .on_hover_text(
                                    "The branch isn't on the remote yet: the push publishes it",
                                );
                        }
                        if !t.tracked {
                            ui.label(egui::RichText::new("sets upstream").color(th.dim))
                                .on_hover_text(
                                    "The branch has no upstream: the push makes this its upstream",
                                );
                        }
                    });
                    ui.add_space(4.0);
                    if o.commits.is_empty() {
                        let text = if o.target.new {
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
                                        ui.label(
                                            egui::RichText::new(hash).color(th.dim).monospace(),
                                        );
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(subject).color(th.text),
                                            )
                                            .truncate(),
                                        );
                                    });
                                }
                                if o.more {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "… and more (only {LISTED} listed)"
                                        ))
                                        .color(th.dim),
                                    );
                                }
                            });
                    }
                }
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(pushable.is_some(), egui::Button::new("Push"))
                    .clicked()
                    && let Some(target) = &pushable
                {
                    choice = Choice::Push(target.clone());
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

    fn never() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }
    /// A repo with one commit, and an empty bare remote (not yet added).
    fn repo_and_remote() -> (tempfile::TempDir, tempfile::TempDir, String) {
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "--bare", "-b", "main"]);
        let repo = tempfile::tempdir().unwrap();
        let dir = repo.path();
        for args in [
            &["init", "-b", "main"][..],
            &["config", "user.name", "Push Test"],
            &["config", "user.email", "push@example.test"],
            &["config", "commit.gpgsign", "false"],
            &["commit", "--allow-empty", "-m", "one"],
        ] {
            git(dir, args);
        }
        let url = remote.path().to_string_lossy().into_owned();
        (repo, remote, url)
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

    #[test]
    fn outgoing_lists_unpushed_commits_and_a_new_branch_target() {
        let (repo, _remote, url) = repo_and_remote();
        let dir = repo.path();
        let err = outgoing(dir, &never()).unwrap_err();
        assert!(err.contains("no remote named origin"), "{err}");
        git(dir, &["remote", "add", "origin", &url]);
        let first = outgoing(dir, &never()).unwrap();
        let t = &first.target;
        assert_eq!(
            (t.branch.as_str(), t.label().as_str(), t.new, t.tracked),
            ("main", "origin/main", true, false)
        );
        assert_eq!(first.commits.len(), 1);
        assert_eq!(first.commits[0].1, "one");
        // The dialog's own target is what gets pushed: it publishes and tracks.
        assert_eq!(push(dir, t, &never()).unwrap(), "Pushed main → origin/main");
        assert_eq!(
            git(dir, &["rev-parse", "--abbrev-ref", "@{u}"]),
            "origin/main"
        );
        git(dir, &["commit", "--allow-empty", "-m", "two"]);
        let tracked = outgoing(dir, &never()).unwrap();
        assert!(!tracked.target.new && tracked.target.tracked);
        let subjects: Vec<_> = tracked.commits.iter().map(|c| c.1.as_str()).collect();
        assert_eq!(subjects, ["two"]);
        git(dir, &["checkout", "-q", "--detach"]);
        let err = outgoing(dir, &never()).unwrap_err();
        assert!(err.starts_with("Detached HEAD"), "{err}");
    }

    #[test]
    fn an_upstream_under_another_name_is_where_both_preview_and_push_go() {
        let (repo, remote, url) = repo_and_remote();
        let dir = repo.path();
        git(dir, &["remote", "add", "origin", &url]);
        git(dir, &["push", "-q", "-u", "origin", "main"]);
        // topic tracks origin/main. A bare `git push` refuses this under the
        // default push.default=simple; the shared target pushes it there.
        git(
            dir,
            &["checkout", "-q", "-b", "topic", "--track", "origin/main"],
        );
        git(dir, &["commit", "--allow-empty", "-m", "on topic"]);
        let preview = outgoing(dir, &never()).unwrap();
        assert_eq!(preview.target.label(), "origin/main");
        assert_eq!(preview.commits.len(), 1);
        assert_eq!(
            push(dir, &preview.target, &never()).unwrap(),
            "Pushed topic → origin/main"
        );
        let bare = remote.path();
        assert_eq!(git(bare, &["log", "-1", "--format=%s", "main"]), "on topic");
        assert_eq!(git(bare, &["branch", "--list", "topic"]), "");
        // A later checkout doesn't redirect a target already shown.
        git(dir, &["checkout", "-q", "-b", "other"]);
        git(dir, &["commit", "--allow-empty", "-m", "on other"]);
        push(dir, &preview.target, &never()).unwrap();
        assert_eq!(git(bare, &["log", "-1", "--format=%s", "main"]), "on topic");
        // Tracking a local branch has no remote to push to.
        git(dir, &["branch", "-q", "-u", "main"]);
        let err = target(dir, &never()).unwrap_err();
        assert!(err.contains("tracks a local branch"), "{err}");
    }

    #[test]
    fn a_rejected_push_says_so() {
        let (one, _remote, url) = repo_and_remote();
        git(one.path(), &["remote", "add", "origin", &url]);
        let t = target(one.path(), &never()).unwrap();
        push(one.path(), &t, &never()).unwrap();
        let (two, _other, _) = repo_and_remote();
        git(two.path(), &["remote", "add", "origin", &url]);
        let t = target(two.path(), &never()).unwrap();
        let err = push(two.path(), &t, &never()).unwrap_err();
        assert!(err.starts_with("Push REJECTED"), "{err}");
    }
}
