//! Which refs the Git History timeline walks. The header dropdown picks a
//! `Scope`; a worker resolves it against the repository into `git log`
//! revisions and a label, and lists the branches the dropdown offers. Git
//! never runs on the GUI thread.
use super::git;
use std::path::Path;
use std::sync::{Arc, atomic::AtomicBool};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);
const CAP: usize = 4 << 20;

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) enum Scope {
    /// `HEAD` plus its upstream: what the human is working on.
    #[default]
    Current,
    Local,
    /// Branches, remotes and tags. Not `--all`, which also walks
    /// `refs/stash` (stash commits draw as fake merge lanes) and other
    /// tools' private refs.
    All,
    /// One branch, by full refname (`refs/heads/…` or `refs/remotes/…`).
    Branch(String),
}

/// A scope after the worker checked it against the repository.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Resolved {
    /// The requested scope, or `Current` when a picked branch is gone.
    pub(super) scope: Scope,
    /// `git log` revision arguments. Empty when there is nothing to walk
    /// (an unborn `HEAD`).
    pub(super) revisions: Vec<String>,
    /// What the closed dropdown shows.
    pub(super) label: String,
}

/// The dropdown's branch list, by full refname, in Git's refname order.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Branches {
    /// Short name of the checked-out branch; `None` when `HEAD` is detached.
    pub(super) head: Option<String>,
    /// Full refname of that branch's upstream.
    pub(super) upstream: Option<String>,
    pub(super) local: Vec<String>,
    /// `refs/heads/card/*`, the kanban board's per-card branches. Listed
    /// apart only to keep the local list short; never hidden.
    pub(super) cards: Vec<String>,
    pub(super) remote: Vec<String>,
}

pub(super) fn revisions(scope: &Scope, has_head: bool, upstream: Option<&str>) -> Vec<String> {
    let rest: Vec<&str> = match scope {
        Scope::Branch(r) => return vec![r.clone()],
        Scope::Current => upstream.into_iter().collect(),
        Scope::Local => vec!["--branches"],
        Scope::All => vec!["--branches", "--remotes", "--tags"],
    };
    // An unborn HEAD would make `git log` fail, so it is left out.
    has_head
        .then_some("HEAD")
        .into_iter()
        .chain(rest)
        .map(String::from)
        .collect()
}

pub(super) fn short(refname: &str) -> &str {
    refname
        .strip_prefix("refs/heads/")
        .or_else(|| refname.strip_prefix("refs/remotes/"))
        .unwrap_or(refname)
}

pub(super) fn label(scope: &Scope, head: Option<&str>) -> String {
    match scope {
        Scope::Current => head.unwrap_or("Detached HEAD").into(),
        Scope::Local => "Local branches".into(),
        Scope::All => "All".into(),
        Scope::Branch(r) => short(r).into(),
    }
}

pub(super) fn current_row(head: Option<&str>, upstream: Option<&str>) -> String {
    match (head, upstream) {
        (None, _) => "Detached HEAD".into(),
        (Some(h), None) => h.into(),
        (Some(h), Some(u)) => format!("{h} → {}", short(u)),
    }
}

fn group(refnames: &str, head: Option<String>, upstream: Option<String>) -> Branches {
    let mut b = Branches {
        head,
        upstream,
        ..Default::default()
    };
    for r in refnames.lines().map(str::trim).filter(|r| !r.is_empty()) {
        if r.starts_with("refs/heads/card/") {
            b.cards.push(r.into());
        } else if r.starts_with("refs/heads/") {
            b.local.push(r.into());
        } else if r.starts_with("refs/remotes/") && !r.ends_with("/HEAD") {
            b.remote.push(r.into());
        }
    }
    b
}

fn text(cwd: &Path, args: &[&str], cancel: &Arc<AtomicBool>) -> Result<String, git::GitError> {
    git::output(cwd, args, cancel, CAP, TIMEOUT)
        .map(|b| String::from_utf8_lossy(&b).trim().to_owned())
}

/// Whether `rev` names a commit. A torn-down card branch does not.
fn exists(cwd: &Path, rev: &str, cancel: &Arc<AtomicBool>) -> bool {
    let spec = format!("{rev}^{{commit}}");
    text(cwd, &["rev-parse", "--verify", "--quiet", &spec], cancel).is_ok()
}

/// The checked-out branch's short name (`None` when detached) and its
/// upstream's full refname, which cannot collide with a local branch name.
fn head_and_upstream(cwd: &Path, cancel: &Arc<AtomicBool>) -> (Option<String>, Option<String>) {
    let head = text(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"], cancel).ok();
    let upstream = head
        .as_ref()
        .and_then(|_| {
            text(
                cwd,
                &["rev-parse", "--symbolic-full-name", "@{upstream}"],
                cancel,
            )
            .ok()
        })
        .filter(|u| !u.is_empty());
    (head, upstream)
}

pub(super) fn resolve(
    cwd: &Path,
    scope: Scope,
    cancel: &Arc<AtomicBool>,
) -> Result<Resolved, String> {
    text(cwd, &["rev-parse", "--git-dir"], cancel).map_err(|e| match e {
        git::GitError::Failed(s) if s.is_empty() => "Not a Git repository".to_string(),
        e => e.to_string(),
    })?;
    let scope = match scope {
        Scope::Branch(r) if !exists(cwd, &r, cancel) => Scope::Current,
        scope => scope,
    };
    let (head, upstream) = head_and_upstream(cwd, cancel);
    Ok(Resolved {
        revisions: revisions(&scope, exists(cwd, "HEAD", cancel), upstream.as_deref()),
        label: label(&scope, head.as_deref()),
        scope,
    })
}

pub(super) fn branches(cwd: &Path, cancel: &Arc<AtomicBool>) -> Result<Branches, String> {
    let refs = text(
        cwd,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/heads",
            "refs/remotes",
        ],
        cancel,
    )
    .map_err(|e| e.to_string())?;
    let (head, upstream) = head_and_upstream(cwd, cancel);
    Ok(group(&refs, head, upstream))
}

/// What a scope's timeline was read from: the tips of the refs it walks,
/// plus `HEAD` (and, for Current, which branch is checked out). When the
/// repository watch says refs moved, a changed fingerprint means the shown
/// timeline is stale. Refs the scope doesn't walk (`refs/stash`, other
/// cards in Current) never count.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Fingerprint {
    pub(super) hash: u64,
    /// Object ids the timeline already covers, for counting new commits.
    pub(super) tips: Vec<String>,
}

const FOR_EACH_REF: &str = "--format=%(objectname)%09%(HEAD)%09%(refname)%09%(upstream)";

/// Pure. `refs` is `git for-each-ref` output in the `FOR_EACH_REF` format;
/// `head` is `HEAD`'s object id (`None` when unborn).
pub(super) fn fingerprint(refs: &str, head: Option<&str>, scope: &Scope) -> Fingerprint {
    let rows: Vec<Vec<&str>> = refs
        .lines()
        .map(|l| l.split('\t').collect::<Vec<_>>())
        .filter(|f| f.len() >= 3)
        .collect();
    let checked_out = rows.iter().find(|f| f[1] == "*");
    let upstream = checked_out.and_then(|f| f.get(3)).filter(|u| !u.is_empty());
    let walks = |name: &str| match scope {
        Scope::Current => upstream.is_some_and(|u| *u == name),
        Scope::Local => name.starts_with("refs/heads/"),
        Scope::All => ["refs/heads/", "refs/remotes/", "refs/tags/"]
            .iter()
            .any(|p| name.starts_with(p)),
        Scope::Branch(r) => name == r,
    };
    let mut lines: Vec<String> = rows
        .iter()
        .filter(|f| walks(f[2]))
        .map(|f| format!("{} {}", f[0], f[2]))
        .collect();
    let mut tips: Vec<String> = rows
        .iter()
        .filter(|f| walks(f[2]))
        .map(|f| f[0].to_string())
        .collect();
    if !matches!(scope, Scope::Branch(_))
        && let Some(head) = head
    {
        lines.push(format!("{head} HEAD"));
        tips.push(head.to_string());
    }
    if *scope == Scope::Current {
        // Switching to a branch at the same commit still changes the label.
        lines.push(format!("* {}", checked_out.map_or("", |f| f[2])));
    }
    lines.sort();
    tips.sort();
    tips.dedup();
    let hash = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        lines.hash(&mut h);
        h.finish()
    };
    Fingerprint { hash, tips }
}

/// Worker-only: the scope's fingerprint as the repository stands now.
pub(super) fn read_fingerprint(
    cwd: &Path,
    scope: &Scope,
    cancel: &Arc<AtomicBool>,
) -> Result<Fingerprint, String> {
    let refs = text(cwd, &["for-each-ref", FOR_EACH_REF], cancel).map_err(|e| e.to_string())?;
    let head = text(cwd, &["rev-parse", "-q", "--verify", "HEAD"], cancel).ok();
    Ok(fingerprint(&refs, head.as_deref(), scope))
}

/// Worker-only: commits `revisions` reach that none of `old` did. `None`
/// when Git can't say (an old tip was pruned, too many tips for one
/// command line).
pub(super) fn new_commits(
    cwd: &Path,
    revisions: &[String],
    old: &[String],
    cancel: &Arc<AtomicBool>,
) -> Option<usize> {
    if revisions.is_empty() || old.len() > 500 {
        return None;
    }
    let mut args = vec!["rev-list", "--count"];
    args.extend(revisions.iter().map(String::as_str));
    args.push("--not");
    args.extend(old.iter().map(String::as_str));
    args.push("--");
    text(cwd, &args, cancel).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_history::tests::git;

    fn cancel() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }
    fn repo_with_root() -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        let dir = repo.path();
        git(dir, &["init", "-b", "main"]);
        git(dir, &["config", "user.name", "Scope Test"]);
        git(dir, &["config", "user.email", "scope@example.test"]);
        git(dir, &["commit", "--allow-empty", "-m", "Root"]);
        repo
    }

    #[test]
    fn scopes_map_to_revisions() {
        let up = Some("refs/remotes/origin/main");
        let cases: &[(Scope, bool, Option<&str>, &[&str])] = &[
            (
                Scope::Current,
                true,
                up,
                &["HEAD", "refs/remotes/origin/main"],
            ),
            (Scope::Current, true, None, &["HEAD"]),
            (Scope::Current, false, None, &[]),
            (Scope::Local, true, up, &["HEAD", "--branches"]),
            (Scope::Local, false, None, &["--branches"]),
            (
                Scope::All,
                true,
                None,
                &["HEAD", "--branches", "--remotes", "--tags"],
            ),
            (
                Scope::All,
                false,
                None,
                &["--branches", "--remotes", "--tags"],
            ),
            (
                Scope::Branch("refs/heads/topic".into()),
                true,
                up,
                &["refs/heads/topic"],
            ),
        ];
        for (scope, has_head, upstream, want) in cases {
            assert_eq!(revisions(scope, *has_head, *upstream), *want, "{scope:?}");
        }
    }

    #[test]
    fn labels_and_the_current_row_read_like_git() {
        assert_eq!(label(&Scope::Current, Some("main")), "main");
        assert_eq!(label(&Scope::Current, None), "Detached HEAD");
        assert_eq!(label(&Scope::Local, Some("main")), "Local branches");
        assert_eq!(label(&Scope::All, None), "All");
        assert_eq!(
            label(&Scope::Branch("refs/remotes/origin/fix/a".into()), None),
            "origin/fix/a"
        );
        assert_eq!(
            label(&Scope::Branch("refs/heads/card/a1".into()), None),
            "card/a1"
        );
        assert_eq!(
            current_row(Some("main"), Some("refs/remotes/origin/main")),
            "main → origin/main"
        );
        assert_eq!(current_row(Some("main"), None), "main");
        assert_eq!(
            current_row(None, Some("refs/remotes/origin/main")),
            "Detached HEAD"
        );
    }

    #[test]
    fn group_splits_local_cards_and_remotes_and_skips_remote_head() {
        let b = group(
            "refs/heads/card/a1\nrefs/heads/feature/ü/x\nrefs/heads/main\n\
             refs/remotes/origin/HEAD\nrefs/remotes/origin/main\n",
            Some("main".into()),
            None,
        );
        assert_eq!(b.local, ["refs/heads/feature/ü/x", "refs/heads/main"]);
        assert_eq!(b.cards, ["refs/heads/card/a1"]);
        assert_eq!(b.remote, ["refs/remotes/origin/main"]);
        assert_eq!(b.head.as_deref(), Some("main"));
        let none = group("refs/heads/main\n", None, None);
        assert!(none.remote.is_empty() && none.cards.is_empty());
    }

    #[test]
    fn resolve_checks_the_repo_and_falls_back_from_a_deleted_branch() {
        let empty = tempfile::tempdir().unwrap();
        assert!(
            resolve(empty.path(), Scope::Current, &cancel()).is_err(),
            "not a repository"
        );
        git(empty.path(), &["init", "-b", "main"]);
        let unborn = resolve(empty.path(), Scope::Current, &cancel()).unwrap();
        assert!(unborn.revisions.is_empty(), "{unborn:?}");
        assert_eq!(unborn.label, "main");

        let repo = repo_with_root();
        let dir = repo.path();
        git(dir, &["branch", "topic"]);
        let topic = Scope::Branch("refs/heads/topic".into());
        let picked = resolve(dir, topic.clone(), &cancel()).unwrap();
        assert_eq!(
            (picked.revisions.clone(), picked.label.as_str()),
            (vec!["refs/heads/topic".to_string()], "topic")
        );
        git(dir, &["branch", "-D", "topic"]);
        let gone = resolve(dir, topic, &cancel()).unwrap();
        assert_eq!(gone.scope, Scope::Current);
        assert_eq!(gone.revisions, ["HEAD"]);
        git(dir, &["checkout", "--detach"]);
        assert_eq!(
            resolve(dir, Scope::Current, &cancel()).unwrap().label,
            "Detached HEAD"
        );
    }

    #[test]
    fn fingerprint_counts_only_the_refs_a_scope_walks() {
        let refs = "a1\t*\trefs/heads/main\trefs/remotes/origin/main\n\
                    b2\t \trefs/heads/card/x\t\n\
                    c3\t \trefs/remotes/origin/main\t\n\
                    d4\t \trefs/tags/v1\t\n\
                    e5\t \trefs/stash\t\n";
        let fp = |refs: &str, head, scope: &Scope| fingerprint(refs, head, scope);
        let current = fp(refs, Some("a1"), &Scope::Current);
        assert_eq!(current.tips, ["a1", "c3"]);
        assert_eq!(fp(refs, Some("a1"), &Scope::Local).tips, ["a1", "b2"]);
        assert_eq!(
            fp(refs, Some("a1"), &Scope::All).tips,
            ["a1", "b2", "c3", "d4"]
        );
        let card = Scope::Branch("refs/heads/card/x".into());
        assert_eq!(fp(refs, Some("a1"), &card).tips, ["b2"]);
        // A stash, or a card moving, leaves Current alone; Local sees the card.
        let moved = refs.replace("e5", "f6").replace("b2", "b9");
        assert_eq!(fp(&moved, Some("a1"), &Scope::Current), current);
        assert_ne!(
            fp(&moved, Some("a1"), &Scope::Local).hash,
            fp(refs, Some("a1"), &Scope::Local).hash
        );
        // HEAD moving (a commit, a detached checkout) changes every HEAD scope.
        assert_ne!(fp(refs, Some("zz"), &Scope::Current).hash, current.hash);
        assert_eq!(fp(refs, Some("zz"), &card), fp(refs, Some("a1"), &card));
        // Switching branch at the same commit changes Current's label.
        let switched = refs.replace("\t*\t", "\t \t").replace("b2\t \t", "b2\t*\t");
        assert_ne!(
            fp(&switched, Some("a1"), &Scope::Current).hash,
            current.hash
        );
        assert_eq!(fp("", None, &Scope::Local).tips, Vec::<String>::new());
    }

    #[test]
    fn new_commits_counts_what_the_old_tips_did_not_reach() {
        let repo = repo_with_root();
        let dir = repo.path();
        let before = read_fingerprint(dir, &Scope::Local, &cancel()).unwrap();
        git(dir, &["commit", "--allow-empty", "-m", "one"]);
        git(dir, &["commit", "--allow-empty", "-m", "two"]);
        git(dir, &["branch", "card/a1"]);
        let after = read_fingerprint(dir, &Scope::Local, &cancel()).unwrap();
        assert_ne!(after.hash, before.hash);
        let local = revisions(&Scope::Local, true, None);
        assert_eq!(new_commits(dir, &local, &before.tips, &cancel()), Some(2));
        assert_eq!(new_commits(dir, &local, &after.tips, &cancel()), Some(0));
        let pruned = ["0".repeat(40)];
        assert_eq!(new_commits(dir, &local, &pruned, &cancel()), None);
        assert_eq!(new_commits(dir, &[], &before.tips, &cancel()), None);
    }

    #[test]
    fn branches_lists_groups_and_the_upstream() {
        let repo = repo_with_root();
        let dir = repo.path();
        git(dir, &["branch", "card/a1"]);
        git(dir, &["branch", "fix/x"]);
        git(dir, &["remote", "add", "origin", "."]);
        git(dir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(dir, &["branch", "--set-upstream-to=origin/main"]);
        let b = branches(dir, &cancel()).unwrap();
        assert_eq!(b.head.as_deref(), Some("main"));
        assert_eq!(b.upstream.as_deref(), Some("refs/remotes/origin/main"));
        assert_eq!(b.local, ["refs/heads/fix/x", "refs/heads/main"]);
        assert_eq!(b.cards, ["refs/heads/card/a1"]);
        assert_eq!(b.remote, ["refs/remotes/origin/main"]);
        let current = resolve(dir, Scope::Current, &cancel()).unwrap();
        assert_eq!(current.revisions, ["HEAD", "refs/remotes/origin/main"]);
    }
}
