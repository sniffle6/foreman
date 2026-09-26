# Git History Branch Selector (Phase 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the Git History header's static "All branches" label with a single-pick scope dropdown (Current / Local / All / one branch), and start subjects after the widest graph *on screen* instead of the widest graph ever loaded.

**Architecture:** A new `src/git_history/scope.rs` owns what refs the timeline walks: a `Scope` enum, a pure scope→revisions mapping, and two worker-thread functions (`resolve`, `branches`) that ask Git about the repo. `stream_history` resolves the scope before it spawns `git log`, and sends the result back on the first `Page`. `HistoryView` draws a themed `egui::ComboBox` and restarts its stream on a pick. The text column is a pure `ease_lanes` function that `HistoryView` calls inside `show_rows` with the visible range's max row width. `Graph` is untouched.

**Tech Stack:** Rust (edition 2024, let-chains in use), egui/eframe 0.34.3, Git CLI via `src/git_history/git.rs` (`git::output`, `git::spawn`), `tempfile` in tests.

**Spec:** `docs/superpowers/specs/2026-09-26-history-branch-selector-design.md` (§1 and §2; §3 is phase 2, not this plan). ADR: `docs/adr/0004-history-graph-streams-with-bounded-lookahead.md` (phase 2 only).

## Global Constraints

- Build and test only with `--target-dir target/agent`. This session may be running inside foreman (`$env:FOREMAN` = `1`); never stop a process named foreman.
- Test command form: `cargo test --target-dir target/agent git_history` (the crate is bin-only; `--lib` fails).
- Git never runs on the GUI thread. Every `git` call goes through `git::output` / `git::spawn` on a worker thread.
- The popup uses the existing theme: `ViewScale::popup_style()`, `selectable_label` rows, theme colors from `crate::theme::live`. No new widget kinds, no checkboxes.
- "All" is `HEAD --branches --remotes --tags` — never `--all` (drops `refs/stash` and foreign namespaces).
- Every scope except One branch includes `HEAD` when `HEAD` has a commit; an unborn `HEAD` is left out.
- Shrink ease: 150 ms (`EASE_S = 0.15`). Growth is instant.
- The branch filter appears only when there are more than 8 branches (`FILTER_MIN = 8`).
- Scope is not persisted: it survives Refresh, not closing the window or restarting.
- Commit only files the task touched, by name (no `git add -A`). Every commit ends with `Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>`.
- Docs cite code by file + symbol, never `file.rs:NNN` (cite-guard hook enforces).

## Review Focus

1. **Empty repo (unborn HEAD) on Current** → "No commits yet.", not a Git error. Pinned by updating `empty_and_non_repository_results_are_distinct` in Task 2 to run with `Scope::Current`.
2. **Detached HEAD in a linked worktree on All** → the detached commit still shows (All must include `HEAD`). Pinned by `worker_reads_real_merge_tags_detached_head_and_worktree_without_writes` running with `Scope::All` in Task 2.
3. **Clicking the filter field** must not close the popup (the default `ComboBox` closes on any click). Pinned in Task 3's dropdown test.
4. **A branch torn down while picked** → next start falls back to Current instead of a `git log` error. Pinned in Task 1's `resolve` test.
5. **A project with no directory** (`HistoryView::new(None)`) → the dropdown still opens with its three scope rows and a dim error line; nothing panics or spawns Git. Pinned in Task 3.

---

### Task 1: `scope.rs` — scopes, revisions, branch list

**Files:**
- Create: `src/git_history/scope.rs`
- Modify: `src/git_history.rs` (add `mod scope;` beside the other `mod` lines at the top)

**Interfaces:**
- Consumes: `super::git::{output, GitError}` (existing: `output(cwd: &Path, args: &[&str], cancel: &Arc<AtomicBool>, cap: usize, timeout: Duration) -> Result<Vec<u8>, GitError>`); test helper `crate::git_history::tests::git(dir, args) -> String`.
- Produces (all `pub(super)`):
  - `enum Scope { Current (Default), Local, All, Branch(String) }` — `Clone, Debug, Default, PartialEq`
  - `struct Resolved { scope: Scope, revisions: Vec<String>, label: String }` — `Clone, Debug, PartialEq`
  - `struct Branches { head: Option<String>, upstream: Option<String>, local: Vec<String>, cards: Vec<String>, remote: Vec<String> }` — `Debug, Default, PartialEq`
  - `fn revisions(scope: &Scope, has_head: bool, upstream: Option<&str>) -> Vec<String>`
  - `fn short(refname: &str) -> &str`
  - `fn label(scope: &Scope, head: Option<&str>) -> String`
  - `fn current_row(head: Option<&str>, upstream: Option<&str>) -> String`
  - `fn resolve(cwd: &Path, scope: Scope, cancel: &Arc<AtomicBool>) -> Result<Resolved, String>`
  - `fn branches(cwd: &Path, cancel: &Arc<AtomicBool>) -> Result<Branches, String>`

- [ ] **Step 1: Write the module skeleton with failing tests**

Create `src/git_history/scope.rs` with the types, the functions stubbed as `todo!()`, and the tests:

```rust
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
    todo!()
}
pub(super) fn short(refname: &str) -> &str {
    todo!()
}
pub(super) fn label(scope: &Scope, head: Option<&str>) -> String {
    todo!()
}
pub(super) fn current_row(head: Option<&str>, upstream: Option<&str>) -> String {
    todo!()
}
fn group(refnames: &str, head: Option<String>, upstream: Option<String>) -> Branches {
    todo!()
}
pub(super) fn resolve(cwd: &Path, scope: Scope, cancel: &Arc<AtomicBool>) -> Result<Resolved, String> {
    todo!()
}
pub(super) fn branches(cwd: &Path, cancel: &Arc<AtomicBool>) -> Result<Branches, String> {
    todo!()
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
            (Scope::Current, true, up, &["HEAD", "refs/remotes/origin/main"]),
            (Scope::Current, true, None, &["HEAD"]),
            (Scope::Current, false, None, &[]),
            (Scope::Local, true, up, &["HEAD", "--branches"]),
            (Scope::Local, false, None, &["--branches"]),
            (Scope::All, true, None, &["HEAD", "--branches", "--remotes", "--tags"]),
            (Scope::All, false, None, &["--branches", "--remotes", "--tags"]),
            (Scope::Branch("refs/heads/topic".into()), true, up, &["refs/heads/topic"]),
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
        assert_eq!(label(&Scope::Branch("refs/heads/card/a1".into()), None), "card/a1");
        assert_eq!(
            current_row(Some("main"), Some("refs/remotes/origin/main")),
            "main → origin/main"
        );
        assert_eq!(current_row(Some("main"), None), "main");
        assert_eq!(current_row(None, Some("refs/remotes/origin/main")), "Detached HEAD");
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
        assert!(resolve(empty.path(), Scope::Current, &cancel()).is_err(), "not a repository");
        git(empty.path(), &["init", "-b", "main"]);
        let unborn = resolve(empty.path(), Scope::Current, &cancel()).unwrap();
        assert!(unborn.revisions.is_empty(), "{unborn:?}");
        assert_eq!(unborn.label, "main");

        let repo = repo_with_root();
        let dir = repo.path();
        git(dir, &["branch", "topic"]);
        let topic = Scope::Branch("refs/heads/topic".into());
        let picked = resolve(dir, topic.clone(), &cancel()).unwrap();
        assert_eq!((picked.revisions.clone(), picked.label.as_str()), (vec!["refs/heads/topic".to_string()], "topic"));
        git(dir, &["branch", "-D", "topic"]);
        let gone = resolve(dir, topic, &cancel()).unwrap();
        assert_eq!(gone.scope, Scope::Current);
        assert_eq!(gone.revisions, ["HEAD"]);
        git(dir, &["checkout", "--detach"]);
        assert_eq!(resolve(dir, Scope::Current, &cancel()).unwrap().label, "Detached HEAD");
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
```

Add `mod scope;` in `src/git_history.rs` after `mod git;`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --target-dir target/agent git_history::scope`
Expected: compiles (unused-variable warnings are fine), all five tests FAIL with `not yet implemented`.

- [ ] **Step 3: Implement the functions**

Replace the `todo!()` bodies:

```rust
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
        .and_then(|_| text(cwd, &["rev-parse", "--symbolic-full-name", "@{upstream}"], cancel).ok())
        .filter(|u| !u.is_empty());
    (head, upstream)
}

pub(super) fn resolve(cwd: &Path, scope: Scope, cancel: &Arc<AtomicBool>) -> Result<Resolved, String> {
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
        &["for-each-ref", "--format=%(refname)", "refs/heads", "refs/remotes"],
        cancel,
    )
    .map_err(|e| e.to_string())?;
    let (head, upstream) = head_and_upstream(cwd, cancel);
    Ok(group(&refs, head, upstream))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --target-dir target/agent git_history::scope`
Expected: 5 passed. (If `branches_lists_groups_and_the_upstream` fails on `--set-upstream-to`, print `git(dir, &["config", "--list"])` and fix the fixture, not the production code.)

- [ ] **Step 5: Commit**

```bash
git add src/git_history/scope.rs src/git_history.rs
git commit -m "feat(history): add scope resolution and branch listing for the timeline

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Stream a resolved scope instead of `--all`

**Files:**
- Modify: `src/git_history.rs` (`Page`, `Stream::start`, `stream_history`, `HistoryView` fields + `new` + `poll`, header label in `HistoryView::show`, tests)

**Interfaces:**
- Consumes: `scope::{Scope, Resolved, resolve}` from Task 1.
- Produces:
  - `Page { rows, end, error, resolved: Option<scope::Resolved> }` — `resolved` is `Some` on the first page of a successful stream only.
  - `Stream::start(cwd: PathBuf, scope: scope::Scope, ctx: egui::Context) -> Stream`
  - `HistoryView` fields `scope: scope::Scope` (starts `Current`) and `label: Option<String>` (the resolved label; `None` until the first page).

- [ ] **Step 1: Update existing tests and add the scope test (failing)**

In the `tests` module of `src/git_history.rs`:

- `worker_reads_real_merge_tags_detached_head_and_worktree_without_writes`: change `Stream::start(work.clone(), egui::Context::default())` to `Stream::start(work.clone(), scope::Scope::All, egui::Context::default())`. (All must include `HEAD`, or the detached commit vanishes.)
- `empty_and_non_repository_results_are_distinct`: change both `Stream::start(repo.path().into(), …)` calls to pass `scope::Scope::Current` as the second argument. (An unborn `HEAD` must still read as empty, not as an error.)
- `demand_batches_keep_graph_continuity_and_stop_when_view_closes`: pass `scope::Scope::Current`.

Add:

```rust
    #[test]
    fn scopes_walk_only_their_refs() {
        let repo = tempfile::tempdir().unwrap();
        let dir = repo.path();
        git(dir, &["init", "-b", "main"]);
        git(dir, &["config", "user.name", "History Test"]);
        git(dir, &["config", "user.email", "history@example.test"]);
        git(dir, &["commit", "--allow-empty", "-m", "Root"]);
        git(dir, &["checkout", "-b", "topic"]);
        git(dir, &["commit", "--allow-empty", "-m", "Topic"]);
        git(dir, &["checkout", "main"]);
        git(dir, &["commit", "--allow-empty", "-m", "Main"]);
        // A foreign namespace and a stash: `--all` walked both.
        let tree = git(dir, &["rev-parse", "HEAD^{tree}"]);
        let private = git(dir, &["commit-tree", &tree, "-m", "Private"]);
        git(dir, &["update-ref", "refs/x/private", &private]);
        std::fs::write(dir.join("wip.txt"), "wip").unwrap();
        git(dir, &["add", "wip.txt"]);
        git(dir, &["stash"]);
        let read = |scope: scope::Scope| {
            let stream = Stream::start(dir.into(), scope, egui::Context::default());
            stream.next.send(()).unwrap();
            let page = stream.pages.recv_timeout(Duration::from_secs(10)).unwrap();
            assert!(page.error.is_none(), "{:?}", page.error);
            let subjects: Vec<String> =
                page.rows.iter().map(|r| r.commit.subject.clone()).collect();
            (subjects, page.resolved.expect("first page carries the scope").label)
        };
        assert_eq!(
            read(scope::Scope::Current),
            (vec!["Main".to_string(), "Root".to_string()], "main".to_string())
        );
        assert_eq!(
            read(scope::Scope::Branch("refs/heads/topic".into())),
            (vec!["Topic".to_string(), "Root".to_string()], "topic".to_string())
        );
        for scope in [scope::Scope::Local, scope::Scope::All] {
            let (subjects, _) = read(scope.clone());
            assert!(subjects.contains(&"Topic".to_string()), "{scope:?}: {subjects:?}");
            assert!(!subjects.contains(&"Private".to_string()), "{scope:?}: {subjects:?}");
            assert!(
                !subjects.iter().any(|s| s.starts_with("WIP on") || s.starts_with("index on")),
                "{scope:?} walked the stash: {subjects:?}"
            );
        }
    }
```

- [ ] **Step 2: Run to verify it fails to compile**

Run: `cargo test --target-dir target/agent git_history`
Expected: compile errors — `Stream::start` takes 2 arguments, `Page` has no field `resolved`.

- [ ] **Step 3: Implement**

`Page` gains the field:

```rust
struct Page {
    rows: Vec<Row>,
    end: bool,
    error: Option<String>,
    /// The scope as the worker resolved it; set on a stream's first page.
    resolved: Option<scope::Resolved>,
}
```

`Stream::start` takes the scope and passes it through; the error page sets `resolved: None`:

```rust
    fn start(cwd: PathBuf, scope: scope::Scope, ctx: egui::Context) -> Self {
        let (next, requests) = mpsc::sync_channel(1);
        let (tx, pages) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        std::thread::spawn(move || {
            let result = stream_history(cwd, scope, &requests, &tx, &stop, &ctx);
            if let Err(error) = result {
                let _ = tx.send(Page {
                    rows: Vec::new(),
                    end: true,
                    error: Some(error),
                    resolved: None,
                });
                ctx.request_repaint();
            }
        });
        Self { next, pages, cancel }
    }
```

`stream_history` resolves first, short-circuits an empty revision list, and appends the revisions before `--`:

```rust
fn stream_history(
    cwd: PathBuf,
    scope: scope::Scope,
    requests: &mpsc::Receiver<()>,
    tx: &mpsc::SyncSender<Page>,
    cancel: &Arc<AtomicBool>,
    ctx: &egui::Context,
) -> Result<(), String> {
    let resolved = scope::resolve(&cwd, scope, cancel)?;
    if resolved.revisions.is_empty() {
        // An unborn HEAD: nothing to walk, and `git log` would fail on it.
        let _ = tx.send(Page {
            rows: Vec::new(),
            end: true,
            error: None,
            resolved: Some(resolved),
        });
        ctx.request_repaint();
        return Ok(());
    }
    let revisions = resolved.revisions.clone();
    let mut args = vec![
        "log",
        "--topo-order",
        "--decorate=short",
        "--no-color",
        "--no-patch",
        "--encoding=UTF-8",
        "--no-show-signature",
        "-z",
        "--format=%H%x00%P%x00%D%x00%an%x00%as%x00%s",
    ];
    args.extend(revisions.iter().map(String::as_str));
    args.push("--");
    let (stdout, exit) = git::spawn(&cwd, &args, cancel, None).map_err(|e| e.to_string())?;
    let mut resolved = Some(resolved);
    // … the existing reader / loop body is unchanged, except the page send:
    //     if tx.send(Page { rows, end, error, resolved: resolved.take() }).is_err() {
```

`HistoryView` gains two fields (initialise `scope: scope::Scope::default()`, `label: None` in `new`):

```rust
    /// What the timeline walks. Survives Refresh; a new window starts on
    /// `Current`. Updated from the worker, which may fall back to `Current`.
    scope: scope::Scope,
    /// The resolved scope's name for the header; `None` until the first page.
    label: Option<String>,
```

In `poll`, start the stream with the scope and adopt the resolved scope from a page:

```rust
                self.stream = Some(Stream::start(cwd.clone(), self.scope.clone(), ctx.clone()));
```

```rust
                Ok(page) => {
                    self.pending = false;
                    self.end = page.end;
                    self.error = page.error;
                    if let Some(resolved) = page.resolved {
                        self.scope = resolved.scope;
                        self.label = Some(resolved.label);
                    }
                    // … existing width/count/pages lines unchanged
```

In `show`, replace the static header text:

```rust
            let label = self.label.clone().unwrap_or_else(|| "…".into());
            ui.label(egui::RichText::new(label).color(th.text).strong());
```

In the Refresh block of `show`, carry the scope and label into the fresh view (after `self.details_w = details_w;`):

```rust
            self.scope = old.scope.clone();
            self.label = old.label.clone();
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --target-dir target/agent git_history`
Expected: all `git_history` tests pass, including `scopes_walk_only_their_refs` and the three updated worker tests.

- [ ] **Step 5: Commit**

```bash
git add src/git_history.rs
git commit -m "feat(history): stream the resolved scope instead of --all

Current (HEAD + upstream) is the default; All drops refs/stash and foreign
namespaces; an unborn HEAD reads as empty rather than a Git error.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: The scope dropdown

**Files:**
- Modify: `src/git_history.rs` (`HistoryView` fields, `new`, `poll`, header in `show`, new `restart`, `load_branches`, `scope_menu`, free fn `menu_row`, tests)

**Interfaces:**
- Consumes: `scope::{Scope, Branches, branches, short, current_row}` (Task 1); `HistoryView.scope` / `.label` (Task 2); `crate::view_scale::ViewScale::{from_ctx, popup_style, factor}`; `crate::theme::live`.
- Produces:
  - `HistoryView::restart(&mut self, ctx: &egui::Context, scope: scope::Scope, keep_selection: bool)` — the one path for Refresh and scope change.
  - Test hooks: `#[cfg(test)] scope_rows: Vec<(String, egui::Rect)>`, `scope_btn: egui::Rect`, `filter_rect: Option<egui::Rect>`.

- [ ] **Step 1: Write the failing tests**

Add shared helpers and two tests to the `tests` module:

```rust
    fn frame(
        ctx: &egui::Context,
        view: &mut HistoryView,
        rect: egui::Rect,
        base: egui::Id,
        events: Vec<egui::Event>,
    ) {
        let _ = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(rect),
                events,
                ..Default::default()
            },
            |ui| view.show(ui, rect, base),
        );
    }
    fn click_at(
        ctx: &egui::Context,
        view: &mut HistoryView,
        rect: egui::Rect,
        base: egui::Id,
        pos: egui::Pos2,
    ) {
        frame(ctx, view, rect, base, vec![egui::Event::PointerMoved(pos)]);
        for pressed in [true, false] {
            let press = egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            frame(ctx, view, rect, base, vec![press]);
        }
    }
    /// Draw frames until `done` holds; workers answer between frames.
    fn settle(
        ctx: &egui::Context,
        view: &mut HistoryView,
        rect: egui::Rect,
        base: egui::Id,
        done: impl Fn(&HistoryView) -> bool,
    ) {
        let start = std::time::Instant::now();
        while !done(view) {
            assert!(start.elapsed() < Duration::from_secs(10), "view never settled");
            frame(ctx, view, rect, base, vec![]);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn scope_dropdown_picks_a_branch_and_keeps_the_selected_commit() {
        let repo = tempfile::tempdir().unwrap();
        let dir = repo.path();
        git(dir, &["init", "-b", "main"]);
        git(dir, &["config", "user.name", "History Test"]);
        git(dir, &["config", "user.email", "history@example.test"]);
        git(dir, &["commit", "--allow-empty", "-m", "Root"]);
        git(dir, &["checkout", "-b", "topic"]);
        git(dir, &["commit", "--allow-empty", "-m", "Topic"]);
        git(dir, &["checkout", "main"]);
        git(dir, &["commit", "--allow-empty", "-m", "Main"]);
        // More than FILTER_MIN branches, so the filter field shows.
        for i in 0..9 {
            git(dir, &["branch", &format!("card/c{i}")]);
        }
        let ctx = egui::Context::default();
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 600.0));
        let base = egui::Id::new("scope-pick");
        let mut view = HistoryView::new(Some(dir.into()));
        settle(&ctx, &mut view, rect, base, |v| v.end);
        assert_eq!(view.label.as_deref(), Some("main"));
        let main = view.pages[0][0].commit.hash.clone();
        view.details.select(dir.into(), main.clone(), ctx.clone());

        click_at(&ctx, &mut view, rect, base, view.scope_btn.center());
        settle(&ctx, &mut view, rect, base, |v| {
            v.scope_rows.iter().any(|(n, _)| n == "topic")
        });
        // No upstream is configured, so the Current row is just the branch.
        assert_eq!(view.scope_rows[0].0, "main");
        // Clicking the filter field must not close the popup.
        let filter = view.filter_rect.expect("more than FILTER_MIN branches show the filter");
        click_at(&ctx, &mut view, rect, base, filter.center());
        frame(&ctx, &mut view, rect, base, vec![]);
        assert!(view.popup_open, "the filter click closed the popup");

        let topic = view.scope_rows.iter().find(|(n, _)| n == "topic").unwrap().1;
        click_at(&ctx, &mut view, rect, base, topic.center());
        assert_eq!(view.scope, scope::Scope::Branch("refs/heads/topic".into()));
        assert_eq!(view.details.selected(), Some(main.as_str()), "details keep the commit");
        settle(&ctx, &mut view, rect, base, |v| v.end);
        let subjects: Vec<&str> =
            view.pages.iter().flatten().map(|r| r.commit.subject.as_str()).collect();
        assert_eq!(subjects, ["Topic", "Root"]);
        assert_eq!(view.label.as_deref(), Some("topic"));
        assert!(!view.popup_open, "a pick closes the popup");
    }

    #[test]
    fn scope_dropdown_without_a_directory_still_offers_the_scopes() {
        let ctx = egui::Context::default();
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 600.0));
        let base = egui::Id::new("scope-none");
        let mut view = HistoryView::new(None);
        frame(&ctx, &mut view, rect, base, vec![]);
        click_at(&ctx, &mut view, rect, base, view.scope_btn.center());
        frame(&ctx, &mut view, rect, base, vec![]);
        let names: Vec<&str> = view.scope_rows.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["Current branch", "Local branches", "All"]);
        assert!(matches!(view.branches, Some(Err(_))), "no Git ran without a directory");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --target-dir target/agent git_history::tests::scope_dropdown`
Expected: compile errors for the missing fields (`scope_btn`, `scope_rows`, `filter_rect`, `popup_open`, `branches`).

- [ ] **Step 3: Implement**

Constants near the top of `src/git_history.rs`:

```rust
/// Branch counts above this show a filter field in the scope dropdown.
const FILTER_MIN: usize = 8;
```

`HistoryView` fields (initialise in `new`: `branches: None, branch_rx: None, popup_open: false, filter: String::new()`, and the test hooks `scope_rows: Vec::new(), scope_btn: egui::Rect::NOTHING, filter_rect: None`):

```rust
    /// The dropdown's branch list, read on a worker each time it opens.
    branches: Option<Result<scope::Branches, String>>,
    branch_rx: Option<mpsc::Receiver<Result<scope::Branches, String>>>,
    /// Whether the dropdown was open last frame, to spot it opening.
    popup_open: bool,
    filter: String,
    #[cfg(test)]
    scope_rows: Vec<(String, egui::Rect)>,
    #[cfg(test)]
    scope_btn: egui::Rect,
    #[cfg(test)]
    filter_rect: Option<egui::Rect>,
```

In `poll`, after the page handling, pick up the branch list:

```rust
        if let Some(rx) = &self.branch_rx
            && let Ok(branches) = rx.try_recv()
        {
            self.branches = Some(branches);
            self.branch_rx = None;
        }
```

New methods on `HistoryView`:

```rust
    /// Start over on `scope`. Retires the old rows off the GUI thread and
    /// keeps the details pane width. A scope change keeps the selected
    /// commit (it is still a valid commit); Refresh clears it.
    fn restart(&mut self, ctx: &egui::Context, scope: scope::Scope, keep_selection: bool) {
        let cwd = self.cwd.clone();
        let generation = self.generation + 1;
        let mut old = std::mem::replace(self, Self::new(cwd));
        self.details_w = old.details_w;
        self.scope = scope;
        // The old name stays until the worker resolves the new scope.
        self.label = old.label.take();
        if keep_selection {
            std::mem::swap(&mut self.details, &mut old.details);
        }
        if let Some(stream) = &old.stream {
            stream.cancel.store(true, Ordering::Relaxed);
        }
        std::thread::spawn(move || drop(old));
        self.generation = generation;
        ctx.request_repaint();
    }

    fn load_branches(&mut self, ctx: &egui::Context) {
        self.filter.clear();
        self.branch_rx = None;
        let Some(cwd) = self.cwd.clone() else {
            self.branches = Some(Err("This project has no directory".into()));
            return;
        };
        self.branches = None;
        let (tx, rx) = mpsc::sync_channel(1);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(scope::branches(&cwd, &Arc::new(AtomicBool::new(false))));
            ctx.request_repaint();
        });
        self.branch_rx = Some(rx);
    }

    /// The dropdown's rows; returns the scope the human clicked.
    fn scope_menu(&mut self, ui: &mut egui::Ui) -> Option<scope::Scope> {
        use scope::Scope;
        let th = crate::theme::live(ui.ctx());
        let s = crate::view_scale::ViewScale::from_ctx(ui.ctx()).factor();
        ui.set_min_width(220.0 * s);
        #[cfg(test)]
        {
            self.scope_rows.clear();
            self.filter_rect = None;
        }
        let mut pick = None;
        let current = match &self.branches {
            Some(Ok(b)) => scope::current_row(b.head.as_deref(), b.upstream.as_deref()),
            _ => "Current branch".to_string(),
        };
        for (scope, text) in [
            (Scope::Current, current),
            (Scope::Local, "Local branches".to_string()),
            (Scope::All, "All".to_string()),
        ] {
            let response = menu_row(ui, self.scope == scope, &text);
            #[cfg(test)]
            self.scope_rows.push((text.clone(), response.rect));
            if response.clicked() {
                pick = Some(scope);
            }
        }
        ui.separator();
        let branches = self.branches.take();
        match &branches {
            None => {
                ui.spinner();
            }
            Some(Err(error)) => {
                ui.colored_label(th.dim, error);
            }
            Some(Ok(b)) => {
                if b.local.len() + b.cards.len() + b.remote.len() > FILTER_MIN {
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut self.filter)
                            .hint_text("filter…")
                            .desired_width(f32::INFINITY),
                    );
                    #[cfg(test)]
                    {
                        self.filter_rect = Some(field.rect);
                    }
                    #[cfg(not(test))]
                    let _ = field;
                }
                let needle = self.filter.to_lowercase();
                egui::ScrollArea::vertical()
                    .max_height(320.0 * s)
                    .show(ui, |ui| {
                        for (heading, refs) in
                            [("LOCAL", &b.local), ("CARDS", &b.cards), ("REMOTE", &b.remote)]
                        {
                            let shown: Vec<&String> = refs
                                .iter()
                                .filter(|r| scope::short(r).to_lowercase().contains(&needle))
                                .collect();
                            if shown.is_empty() {
                                continue;
                            }
                            ui.label(egui::RichText::new(heading).small().color(th.dim));
                            for r in shown {
                                let scope = Scope::Branch(r.clone());
                                let response = menu_row(ui, self.scope == scope, scope::short(r));
                                #[cfg(test)]
                                self.scope_rows.push((scope::short(r).to_string(), response.rect));
                                if response.clicked() {
                                    pick = Some(scope);
                                }
                            }
                        }
                    });
            }
        }
        self.branches = branches;
        if pick.is_some() {
            ui.close();
        }
        pick
    }
```

Free function beside `meta_w`:

```rust
/// One dropdown row: the theme's selectable label, with a check on the
/// current choice.
fn menu_row(ui: &mut egui::Ui, on: bool, text: &str) -> egui::Response {
    ui.selectable_label(on, if on { format!("{text}  ✓") } else { text.to_owned() })
}
```

Note: `menu_row` appends `  ✓` to the *painted* text, but the tests record the plain `text` in `scope_rows` — keep it that way. If the ✓ glyph renders as tofu in the Task 5 screenshot, switch to `✔` (egui's bundled emoji font has it).

In `show`, replace the header label from Task 2 and the Refresh block. The `horizontal` closure becomes:

```rust
        let mut refresh = false;
        let mut pick = None;
        child.horizontal(|ui| {
            let label = self.label.clone().unwrap_or_else(|| "…".into());
            let combo = egui::ComboBox::from_id_salt(base.with("scope"))
                .selected_text(egui::RichText::new(label).color(th.text).strong())
                // Clicks in the filter field must not close it; rows close it
                // themselves with `ui.close()`.
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .popup_style(zoom.popup_style())
                .show_ui(ui, |ui| self.scope_menu(ui));
            #[cfg(test)]
            {
                self.scope_btn = combo.response.rect;
            }
            let open = combo.inner.is_some();
            if open && !self.popup_open {
                self.load_branches(ui.ctx());
            }
            self.popup_open = open;
            pick = combo.inner.flatten();
            // … the existing commit-count label, spinner, and Refresh button
            //    stay exactly as they are.
        });
        if refresh {
            self.restart(child.ctx(), self.scope.clone(), false);
            return;
        }
        if let Some(scope) = pick
            && scope != self.scope
        {
            self.restart(child.ctx(), scope, true);
            return;
        }
```

Delete the old inline Refresh body (the `std::mem::replace` block and the `self.scope = old.scope.clone(); self.label = …` lines Task 2 added): `restart` now owns it. `clicking_rows_changes_selection_and_refresh_clears_it` pins that Refresh still clears the selection. Its Refresh click coordinate `(180.0, 18.0)` assumed the old label width; if that test now misses the button, record the Refresh button rect in a `#[cfg(test)] refresh_btn: egui::Rect` field and click its center instead of adjusting the magic number.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --target-dir target/agent git_history`
Expected: all pass, including both `scope_dropdown_*` tests and `clicking_rows_changes_selection_and_refresh_clears_it`.

- [ ] **Step 5: Commit**

```bash
git add src/git_history.rs
git commit -m "feat(history): add the scope dropdown to the Git History header

Single-pick themed ComboBox: Current, Local branches, All, then branches
grouped Local / Cards / Remote with a filter past eight. A pick restarts
the stream and keeps the selected commit; Refresh keeps the scope.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Per-screen text column with eased shrink

**Files:**
- Modify: `src/git_history.rs` (constant `EASE_S`, free fn `ease_lanes`, `HistoryView` fields, `poll`, the timeline part of `show`, tests)

**Interfaces:**
- Consumes: `Row::width` (existing); `frame` / `settle` test helpers (Task 3).
- Produces: `fn ease_lanes(shown: f32, rate: f32, target: f32, dt: f32) -> (f32, f32)`; `HistoryView` fields `lanes: f32` (starts `1.0`), `shrink_rate: f32` (starts `0.0`), `#[cfg(test)] drawn_graph_w: f32`. The `width: usize` field is removed.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn text_column_grows_at_once_and_eases_back() {
        let dt = 1.0 / 60.0;
        assert_eq!(ease_lanes(2.0, 0.0, 6.0, dt), (6.0, 0.0), "growth is instant");
        assert_eq!(ease_lanes(3.0, 0.0, 3.0, dt), (3.0, 0.0), "steady state");
        assert_eq!(ease_lanes(4.0, 9.0, 5.0, dt), (5.0, 0.0), "growth cancels a shrink");
        let (mut lanes, mut rate, mut frames) = (6.0f32, 0.0f32, 0);
        while lanes > 2.0 {
            let (next, r) = ease_lanes(lanes, rate, 2.0, dt);
            assert!(next < lanes && next >= 2.0, "{lanes} -> {next}");
            (lanes, rate, frames) = (next, r, frames + 1);
            assert!(frames <= 10, "shrink took longer than EASE_S");
        }
        // 0.15 s at 60 fps is 9 frames; float rounding may add one.
        assert!((9..=10).contains(&frames), "{frames} frames");
        assert_eq!(rate, 0.0, "a finished shrink forgets its rate");
    }

    #[test]
    fn subjects_start_after_the_widest_graph_on_screen() {
        let mut view = HistoryView::new(None);
        view.end = true;
        let mut g = Graph::default();
        let mut rows = Vec::new();
        // 100 linear rows (1 lane), an octopus opening 6 lanes for ~60 rows,
        // then 200 linear rows again.
        for i in 0..100 {
            let parent = if i < 99 { format!("a{}", i + 1) } else { "m".into() };
            rows.push(g.push(commit(&format!("a{i}"), &[&parent])));
        }
        let heads: Vec<String> = (0..6).map(|k| format!("b{k}_0")).collect();
        let heads: Vec<&str> = heads.iter().map(String::as_str).collect();
        rows.push(g.push(commit("m", &heads)));
        for k in 0..6 {
            for j in 0..10 {
                let parent = if j < 9 { format!("b{k}_{}", j + 1) } else { "r".into() };
                rows.push(g.push(commit(&format!("b{k}_{j}"), &[&parent])));
            }
        }
        rows.push(g.push(commit("r", &["c0"])));
        for i in 0..200 {
            let parent = format!("c{}", i + 1);
            let parents: &[&str] = if i < 199 { &[&parent] } else { &[] };
            rows.push(g.push(commit(&format!("c{i}"), parents)));
        }
        view.count = rows.len();
        view.pages.push(rows);
        let ctx = egui::Context::default();
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 600.0));
        let base = egui::Id::new("column");
        let narrow = 2.0 * LANE_W;
        let wheel = |dy: f32| {
            vec![
                egui::Event::PointerMoved(egui::pos2(200.0, 300.0)),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    phase: egui::TouchPhase::Move,
                    delta: egui::vec2(0.0, dy),
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        };
        frame(&ctx, &mut view, rect, base, vec![]);
        assert_eq!(view.drawn_graph_w, narrow, "top of history is one lane");
        // Scroll into the octopus stretch (row 101 onward).
        frame(&ctx, &mut view, rect, base, wheel(-104.0 * ROW_H));
        settle(&ctx, &mut view, rect, base, |v| v.drawn.start >= 100);
        frame(&ctx, &mut view, rect, base, vec![]);
        assert!(view.drawn.start < 150, "{:?}", view.drawn);
        assert_eq!(view.drawn_graph_w, 7.0 * LANE_W, "six lanes on screen, instantly");
        // Back to the top: the column glides back within the ease.
        frame(&ctx, &mut view, rect, base, wheel(200.0 * ROW_H));
        settle(&ctx, &mut view, rect, base, |v| v.drawn.start == 0);
        // Headless frames advance `stable_dt` by egui's default 1/60 s, so
        // 15 frames (0.25 s) outlast the 0.15 s ease.
        for _ in 0..15 {
            frame(&ctx, &mut view, rect, base, vec![]);
        }
        assert_eq!(view.drawn_graph_w, narrow);
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --target-dir target/agent git_history::tests::text_column git_history::tests::subjects_start`
(If cargo rejects two filters, run `cargo test --target-dir target/agent git_history` and look for the two names.)
Expected: compile errors — `ease_lanes` and `drawn_graph_w` do not exist.

- [ ] **Step 3: Implement**

Constant beside `FADE_W`:

```rust
/// Seconds the subject column takes to slide back left after a wide
/// stretch of graph scrolls off screen.
const EASE_S: f32 = 0.15;
```

Free function beside `meta_w`:

```rust
/// Lanes of graph room to show this frame, and the shrink rate to carry to
/// the next. Growth is instant so a wider row never paints under text. A
/// shrink runs at the speed that closes the largest gap seen in `EASE_S`,
/// so the column glides back instead of jumping.
fn ease_lanes(shown: f32, rate: f32, target: f32, dt: f32) -> (f32, f32) {
    if target >= shown {
        return (target, 0.0);
    }
    let rate = rate.max((shown - target) / EASE_S);
    let next = (shown - rate * dt).max(target);
    (next, if next > target { rate } else { 0.0 })
}
```

`HistoryView`: delete the `width: usize` field (and `width: 1` in `new`, and the `self.width = …` update in `poll`). Add, initialised to `1.0`, `0.0`, `0.0`:

```rust
    /// Graph room shown, in lanes: the widest row on screen, eased down.
    lanes: f32,
    shrink_rate: f32,
    #[cfg(test)]
    drawn_graph_w: f32,
```

`restart` needs no change (a fresh `Self::new` resets both).

In `show`, the graph width now depends on the visible range, so it moves inside `show_rows`. Before the scroll area, replace the `graph_w` / `total_w` lines with:

```rust
        let (row_h, lane_w) = (zoom.px(ROW_H), zoom.px(LANE_W));
        let dt = ui.input(|i| i.stable_dt).min(0.1);
        let avail_w = child.available_width() - 12.0 * s;
```

(keep `font` and `date_w` as they are). At the top of the `show_rows` closure, before `ui.set_min_width`:

```rust
            let target = range
                .clone()
                .map(|i| self.pages[i / BATCH][i % BATCH].width)
                .max()
                .unwrap_or(1) as f32;
            (self.lanes, self.shrink_rate) = ease_lanes(self.lanes, self.shrink_rate, target, dt);
            if self.shrink_rate > 0.0 {
                ui.ctx().request_repaint();
            }
            let graph_w = (self.lanes + 1.0) * lane_w;
            #[cfg(test)]
            {
                self.drawn_graph_w = graph_w;
            }
            // Only a lane graph wider than the pane scrolls sideways; the
            // metadata stays pinned to the visible edge either way.
            let total_w = (graph_w + MIN_SUBJECT_W * s + meta_w(date_w, s)).max(avail_w);
            ui.set_min_width(total_w);
```

Remove the now-duplicate `ui.set_min_width(total_w);` line that followed. The rest of the row loop already reads `graph_w` and `total_w` by name and needs no change.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --target-dir target/agent git_history`
Expected: all pass, including `large_history_paints_only_viewport_rows_and_scrolls_to_old_commits` (painting is still viewport-only) and `narrow_pane_keeps_subjects_whole_and_drops_metadata_first`.

- [ ] **Step 5: Commit**

```bash
git add src/git_history.rs
git commit -m "feat(history): start subjects after the widest graph on screen

The graph column was the widest row ever loaded, so one busy stretch pushed
every subject right. It now tracks the visible rows: growth is instant so
lanes never paint under text, and a shrink eases over 150 ms.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: Feature doc and visual check

**Files:**
- Modify: `docs/git-history.md`

**Interfaces:**
- Consumes: the shipped behavior of Tasks 1–4.
- Produces: the doc of record for phase 1.

- [ ] **Step 1: Update `docs/git-history.md`**

In `## What it does`, replace the sentence "The timeline shows commits reachable from all refs and HEAD in topological order, with …" through "… commit subjects," so it opens with the scope:

```markdown
The timeline shows commits in topological order, with colored branch/merge
lanes, commit subjects, branch/tag decorations, authors, and author dates.
Which commits it walks is the **scope**, picked from the dropdown at the left
of the header:

- **Current** (the default, shown as the branch name): `HEAD` plus its
  upstream. `Detached HEAD` when detached.
- **Local branches**: `HEAD` and every local branch, `card/*` included.
- **All**: `HEAD`, local and remote branches, and tags. Unlike `git log
  --all` it skips `refs/stash` (stash commits draw as fake merges) and other
  tools' private refs.
- **One branch**: any branch from the list below the scopes, grouped Local /
  Cards / Remote, with a filter once there are more than eight.

The branch list is read fresh each time the dropdown opens. Picking restarts
the read and keeps the selected commit in the details pane. The scope
survives Refresh but not closing the window: a new History window starts on
Current. If a picked branch is deleted (a finished card), the next read falls
back to Current. Resolution lives in `src/git_history/scope.rs` (`resolve`,
`revisions`).
```

After the paragraph ending "… rows with long subjects show no metadata.", add:

```markdown
Subjects start after the widest lane graph **on screen**, not the widest in
the whole history, so a busy stretch only pushes text right while it is
visible. Growth is instant (lanes never paint under text); when the wide
rows scroll away the column glides back over 150 ms (`ease_lanes`).
```

Delete the sentence "Branch selection, checkout, search, context menus, and other Git operations are outside this viewer's scope." and write instead:

```markdown
Checkout, search, context menus, and other Git operations are outside this
viewer's scope.
```

In `## Key files`, add after the `src/git_history.rs` bullet:

```markdown
- `src/git_history/scope.rs`: `Scope`, `resolve` (scope → `git log`
  revisions and header label, with the deleted-branch fallback), and
  `branches` (the dropdown's grouped branch list).
```

and extend the `src/git_history.rs` bullet to mention `ease_lanes` and `HistoryView::restart`.

- [ ] **Step 2: Run the citation guard**

Run: `pwsh -NoProfile -File .claude/hooks/cite-guard.ps1 -All`
Expected: `cite-guard: clean`.

- [ ] **Step 3: Full test pass and warning check**

Run: `cargo test --target-dir target/agent git_history`
Expected: all pass.
Run: `cargo build --target-dir target/agent 2>&1 | Select-String -Pattern 'warning'`
Expected: no new warnings from `src/git_history*`.

- [ ] **Step 4: Visual check (the user runs it)**

The GUI cannot be seen from the terminal, and **build-screenshot** is user-only. Ask the user to run it (or `.\scripts\run-dev.ps1 -Debug -SeedWorkspace`, see `docs/dev-launcher.md`) against `H:\claude code\epic-manager` and check:
- The closed dropdown shows `main ▾` and the header still fits at default and 2× zoom.
- The open popup: three scope rows, a separator, dim `LOCAL` / `CARDS` / `REMOTE` headings, the filter field (epic-manager has more than eight branches), ✓ on the current choice rendering as a check and not tofu.
- On **All**, scrolling through a busy region moves subjects right while it is visible and glides back after.

Record the result (date, what was checked) in the `## Validation` section of `docs/git-history.md`.

- [ ] **Step 5: Commit**

```bash
git add docs/git-history.md
git commit -m "docs(history): document the scope dropdown and per-screen text column

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

- [ ] **Step 6: Delete this plan once phase 1 has shipped**

Per `docs/superpowers/README.md`, a shipped plan is deleted; the feature doc is the explanation. `git rm docs/superpowers/plans/2026-09-26-history-branch-selector-phase1.md` in the same or a follow-up `docs` commit.
