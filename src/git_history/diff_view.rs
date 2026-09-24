//! The Diff window: a reusable per-Project side-by-side view of one file's
//! change at one commit, loaded on a cancellable worker.
use super::diff::{self, Diff, Notice};
use super::git;
use std::path::Path;
use std::sync::{Arc, atomic::AtomicBool};
use std::time::Duration;

/// One file's change at one commit. Built by the details pane; persisted in
/// `ContentSnap::GitDiff`, so it carries no live state.
#[derive(Clone, Debug, PartialEq)]
pub struct DiffTarget {
    pub commit: String,
    /// First parent; `None` for a root commit.
    pub parent: Option<String>,
    /// `A M D R C T`, as in the details pane.
    pub status: char,
    /// Source path for renames and copies.
    pub old_path: Option<String>,
    pub path: String,
    pub merge: bool,
}

fn is_object_id(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

impl DiffTarget {
    pub fn file_name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
    fn versus(&self) -> String {
        let short = |h: &str| h.get(..7).unwrap_or(h).to_owned();
        match (self.status, &self.parent) {
            ('A', _) | (_, None) => format!("{} · new file", short(&self.commit)),
            ('D', _) => format!("{} · deleted file", short(&self.commit)),
            (_, Some(p)) if self.merge => {
                format!("{} vs {} (first parent)", short(&self.commit), short(p))
            }
            (_, Some(p)) => format!("{} vs {}", short(&self.commit), short(p)),
        }
    }
    /// Git arguments for this change. A/D/M use a pathspec-limited diff-tree
    /// (a submodule shows its gitlink header). T/R/C use the blob form, which
    /// pairs renamed/copied paths exactly; `load` resolves a T pair to bare
    /// blob ids so the mode change does not split it into delete + add.
    fn args(&self) -> Result<Vec<String>, String> {
        if !is_object_id(&self.commit) || self.parent.as_deref().is_some_and(|p| !is_object_id(p)) {
            return Err("Invalid commit id".into());
        }
        let mut args: Vec<String> = Vec::new();
        let flags = [
            "--no-ext-diff".to_owned(),
            "--no-textconv".into(),
            "--no-color".into(),
            format!("-U{}", diff::CONTEXT_LINES),
        ];
        match (self.status, &self.parent) {
            ('T' | 'R' | 'C', Some(parent)) => {
                let old = self.old_path.as_deref().unwrap_or(&self.path);
                args.push("diff".into());
                args.extend(flags);
                args.push(format!("{parent}:{old}"));
                args.push(format!("{}:{}", self.commit, self.path));
            }
            ('A' | 'D' | 'M', Some(parent)) => {
                args.extend(["diff-tree".into(), "-p".into()]);
                args.extend(flags);
                args.extend([
                    parent.clone(),
                    self.commit.clone(),
                    "--".into(),
                    self.path.clone(),
                ]);
            }
            (_, None) => {
                args.extend(["diff-tree".into(), "-p".into(), "--root".into()]);
                args.extend(flags);
                args.extend([self.commit.clone(), "--".into(), self.path.clone()]);
            }
            _ => return Err(format!("Unsupported change type {}", self.status)),
        }
        Ok(args)
    }
}

/// Worker-only: run the diff read and parse it.
pub(super) fn load(
    cwd: &Path,
    target: &DiffTarget,
    cancel: &Arc<AtomicBool>,
) -> Result<Diff, String> {
    let mut args = target.args()?;
    if target.status == 'T' && target.parent.is_some() {
        // git 2.39 still splits a `rev:path` pair whose modes differ into a
        // delete plus an add. Bare blob ids carry no mode: one content hunk.
        let specs = args.split_off(args.len() - 2);
        args.extend(blob_ids(cwd, &specs, cancel)?);
    }
    match run(cwd, &args, cancel, 16 << 20) {
        Ok(bytes) => diff::parse(&bytes),
        Err(git::GitError::TooLarge) => Ok(Diff::Notice(Notice::TooLarge)),
        Err(e) => Err(message(e)),
    }
}

fn run(
    cwd: &Path,
    args: &[String],
    cancel: &Arc<AtomicBool>,
    cap: usize,
) -> Result<Vec<u8>, git::GitError> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    git::output(cwd, &args, cancel, cap, Duration::from_secs(30))
}

fn message(e: git::GitError) -> String {
    match e {
        git::GitError::Failed(stderr) if stderr.is_empty() => "Git could not read this diff".into(),
        e => e.to_string(),
    }
}

/// Resolve `rev:path` specs to their object ids.
fn blob_ids(cwd: &Path, specs: &[String], cancel: &Arc<AtomicBool>) -> Result<Vec<String>, String> {
    let mut args = vec!["rev-parse".to_owned()];
    args.extend_from_slice(specs);
    let bytes = run(cwd, &args, cancel, 4096).map_err(message)?;
    let ids: Vec<String> = String::from_utf8_lossy(&bytes)
        .lines()
        .map(str::to_owned)
        .collect();
    if ids.len() != specs.len() || !ids.iter().all(|id| is_object_id(id)) {
        return Err("Git could not read this diff".into());
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_history::tests::git;

    fn repo() -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        for args in [
            &["init", "-b", "main"][..],
            &["config", "user.name", "Diff Test"],
            &["config", "user.email", "diff@example.test"],
            &["config", "commit.gpgsign", "false"],
            &["config", "core.autocrlf", "false"],
        ] {
            git(repo.path(), args);
        }
        repo
    }
    fn lines(n: usize) -> String {
        (1..=n).map(|i| format!("line {i}\n")).collect()
    }
    fn flag() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }
    fn target(dir: &Path, status: char, old: Option<&str>, path: &str) -> DiffTarget {
        DiffTarget {
            commit: git(dir, &["rev-parse", "HEAD"]),
            parent: Some(git(dir, &["rev-parse", "HEAD^"])),
            status,
            old_path: old.map(Into::into),
            path: path.into(),
            merge: false,
        }
    }
    fn doc(r: Result<Diff, String>) -> diff::Doc {
        match r.unwrap() {
            Diff::Doc(d) => d,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn real_changes_of_every_status_load_as_side_by_side_rows() {
        let repo = repo();
        let dir = repo.path();
        std::fs::write(dir.join("m.txt"), lines(1000)).unwrap();
        std::fs::write(dir.join("d.txt"), "gone\n").unwrap();
        std::fs::write(dir.join("old.txt"), lines(50)).unwrap();
        std::fs::write(dir.join("same.txt"), "unchanged\n").unwrap();
        std::fs::write(dir.join("t.txt"), "plain\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "base"]);
        let root = DiffTarget {
            commit: git(dir, &["rev-parse", "HEAD"]),
            parent: None,
            status: 'A',
            old_path: None,
            path: "d.txt".into(),
            merge: false,
        };
        let d = doc(load(dir, &root, &flag()));
        assert_eq!(d.rows.len(), 1);
        assert_eq!(d.rows[0].kind, diff::Kind::Added);

        // Edits 890 lines apart must still be one whole-file hunk (bounded -U).
        let m = lines(1000)
            .replace("line 10\n", "line ten\n")
            .replace("line 900\n", "line nine hundred\n");
        std::fs::write(dir.join("m.txt"), m).unwrap();
        std::fs::remove_file(dir.join("d.txt")).unwrap();
        std::fs::write(dir.join("a.txt"), "new\nfile\n").unwrap();
        git(dir, &["mv", "old.txt", "new.txt"]);
        std::fs::write(
            dir.join("new.txt"),
            lines(50).replace("line 25\n", "line 25!\n"),
        )
        .unwrap();
        git(dir, &["mv", "same.txt", "same2.txt"]);
        git(dir, &["add", "-A", "."]);
        // Type change without a real symlink: stage mode 120000 directly.
        std::fs::write(dir.join("link.tmp"), "link-target").unwrap();
        let blob = git(dir, &["hash-object", "-w", "link.tmp"]);
        std::fs::remove_file(dir.join("link.tmp")).unwrap();
        git(
            dir,
            &[
                "update-index",
                "--cacheinfo",
                &format!("120000,{blob},t.txt"),
            ],
        );
        git(dir, &["commit", "-m", "second"]);
        let before = git(dir, &["status", "--porcelain=v1"]);

        let m = doc(load(dir, &target(dir, 'M', None, "m.txt"), &flag()));
        assert_eq!(m.rows.len(), 1000);
        assert_eq!(m.blocks.len(), 2);
        assert_eq!(m.blocks[0].rows, 9..10);
        let row = &m.rows[9];
        assert_eq!(row.kind, diff::Kind::Modified);
        let new = row.new.as_ref().unwrap();
        assert_eq!(&new.text[new.hot.clone().unwrap()], "ten");

        let d = doc(load(dir, &target(dir, 'D', None, "d.txt"), &flag()));
        assert_eq!(d.rows[0].kind, diff::Kind::Removed);
        let a = doc(load(dir, &target(dir, 'A', None, "a.txt"), &flag()));
        assert_eq!(a.rows.len(), 2);
        let r = doc(load(
            dir,
            &target(dir, 'R', Some("old.txt"), "new.txt"),
            &flag(),
        ));
        assert_eq!(r.blocks.len(), 1);
        assert_eq!(r.blocks[0].rows, 24..25);
        let c = doc(load(
            dir,
            &target(dir, 'C', Some("old.txt"), "new.txt"),
            &flag(),
        ));
        assert_eq!(c.blocks, r.blocks);
        assert_eq!(
            load(
                dir,
                &target(dir, 'R', Some("same.txt"), "same2.txt"),
                &flag()
            )
            .unwrap(),
            Diff::Notice(Notice::Unchanged)
        );
        let t = doc(load(dir, &target(dir, 'T', None, "t.txt"), &flag()));
        assert_eq!(t.blocks.len(), 1);
        assert_eq!(
            git(dir, &["status", "--porcelain=v1"]),
            before,
            "diff reads must not write"
        );
    }

    #[test]
    fn files_over_the_line_cap_are_a_notice_not_a_partial_render() {
        let repo = repo();
        let dir = repo.path();
        let n = diff::CONTEXT_LINES + 2;
        std::fs::write(dir.join("big.txt"), lines(n)).unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "base"]);
        // Only the last line changes, so the hunk would start at line
        // n - CONTEXT_LINES = 2: git cannot return the whole file as one hunk.
        // (A change at BOTH ends would be one complete hunk, correctly rendered.)
        let edited = lines(n).replace(&format!("line {n}\n"), "last\n");
        std::fs::write(dir.join("big.txt"), edited).unwrap();
        git(dir, &["commit", "-am", "edit ends"]);
        assert_eq!(
            load(dir, &target(dir, 'M', None, "big.txt"), &flag()).unwrap(),
            Diff::Notice(Notice::TooLarge)
        );
    }

    #[test]
    fn missing_commits_and_bad_ids_are_errors_not_panics() {
        let repo = repo();
        let dir = repo.path();
        git(dir, &["commit", "--allow-empty", "-m", "root"]);
        let mut t = DiffTarget {
            commit: "0".repeat(40),
            parent: Some("1".repeat(40)),
            status: 'M',
            old_path: None,
            path: "x.txt".into(),
            merge: false,
        };
        assert!(!load(dir, &t, &flag()).unwrap_err().is_empty());
        t.commit = "--output=pwned".into();
        assert_eq!(load(dir, &t, &flag()).unwrap_err(), "Invalid commit id");
        let not_repo = tempfile::tempdir().unwrap();
        t.commit = "0".repeat(40);
        assert!(load(not_repo.path(), &t, &flag()).is_err());
    }

    #[test]
    fn target_labels() {
        let t = DiffTarget {
            commit: "abcdef0123".repeat(4),
            parent: Some("1234567890".repeat(4)),
            status: 'M',
            old_path: None,
            path: "src/dir/file.rs".into(),
            merge: true,
        };
        assert_eq!(t.file_name(), "file.rs");
        assert_eq!(t.versus(), "abcdef0 vs 1234567 (first parent)");
        let added = DiffTarget {
            status: 'A',
            parent: None,
            merge: false,
            ..t
        };
        assert_eq!(added.versus(), "abcdef0 · new file");
    }
}
