# Git History Diff Window Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Clicking a changed file in the Git History details pane opens a reusable per-Project side-by-side Diff window showing the whole file, old vs new.

**Architecture:** A shared subprocess helper (`git.rs`) replaces the two copies of git spawning. Git computes the diff with bounded full context; a pure parser (`diff.rs`) turns the single whole-file hunk into padded, aligned rows and blocks. `DiffView` (`diff_view.rs`) loads on a cancellable worker and paints only visible rows. The details pane records the click, `HistoryView.acts` carries it out of the draw pass, and `WindowManager` opens or retargets the one `Content::GitDiff` window.

**Tech Stack:** Rust 2024, egui 0.34.3 (eframe/glow), git CLI on PATH. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-09-23-diff-window-design.md`

## Global Constraints

- No new crate dependency. No syntax highlighting, curved connectors, or dropdowns.
- The GUI thread never waits on git: every git call runs on a worker; the GUI only `try_recv`s.
- Read-only: no repository writes. Every git call sets `GIT_OPTIONAL_LOCKS=0` and `GIT_TERMINAL_PROMPT=0` (the helper does this).
- Diff reads: `--no-ext-diff --no-textconv --no-color -U200000`, output cap 16 MiB, timeout 30 s.
- Any frame-time panic aborts the whole app (every terminal dies). Slice strings only on char boundaries; index rows only within `rows.len()`.
- New egui Ids derive from the `base` Id passed into `show`, never from a bare `WinId`.
- We run INSIDE foreman (`$env:FOREMAN` = `1`). Run cargo through the **PowerShell tool** with `--target-dir target/agent`. Never `Stop-Process foreman`; never run cargo through the Bash tool (its hook kills `target\` foreman builds, which may be the host).
- Commits: stage explicit paths only (never `git add -A`, never `.foreman/`). Message ends with:
  ```
  Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
  Card: l8th0t
  ```
- Test commands: `cargo test --target-dir target/agent git_history` covers `git_history`, `details`, `diff`, `diff_view`, and `git` modules. Run `cargo fmt` before each commit.

## Review Focus

1. **A single enormous line** (minified JS, a lockfile line of 1 MB): painting must lay out only the visible slice of a line, not the whole line, every frame. Pinned by `visible_slice_*` tests in Task 4.
2. **Multibyte text under horizontal scroll and word highlight** (`é`, CJK, emoji): slicing on a non-char boundary panics the frame and kills the app. Pinned by `visible_slice_respects_char_boundaries` (Task 4) and `trim_spans_multibyte_chars` (Task 2).
3. **A restored Diff window whose commit no longer exists** (gc, rebased branch) or whose Project directory is no longer a repository: one error line, no panic, no retry loop. Pinned by `missing_commits_and_bad_ids_are_errors_not_panics` (Task 3).
4. **Clicking another file while a slow diff is loading**: the old result must never appear for the new file. Pinned by `retarget_cancels_the_previous_request` (Task 4).
5. **Only the trailing newline changed, or CRLF→LF across a whole file**: shows as Modified rows with nothing hot. Must not panic on an empty trim or read as "no changes". Pinned by `no_newline_marker_and_crlf` (Task 2).

---

### Task 1: Shared git subprocess helper

Behavior-preserving extraction. The existing `git_history` tests are the gate.

**Files:**
- Create: `src/git_history/git.rs`
- Modify: `src/git_history.rs` (`mod` list, `stream_history`, imports)
- Modify: `src/git_history/details.rs` (`git_output`, `load` signature, tests)

**Interfaces:**
- Produces:
  - `pub(super) enum GitError { Spawn(String), Cancelled, TimedOut, TooLarge, Failed(String), Io(String) }` with `Debug, PartialEq` and `Display`
  - `pub(super) fn spawn(cwd: &Path, args: &[&str], cancel: &Arc<AtomicBool>, timeout: Option<Duration>) -> Result<(ChildStdout, Exit), GitError>`
  - `pub(super) struct Exit` with `pub(super) fn finish(self) -> Result<(), GitError>`
  - `pub(super) fn output(cwd: &Path, args: &[&str], cancel: &Arc<AtomicBool>, cap: usize, timeout: Duration) -> Result<Vec<u8>, GitError>`
  - `details::load(cwd: &Path, hash: &str, cancel: &Arc<AtomicBool>)` (was `&AtomicBool`)

- [ ] **Step 1: Write the failing tests** in the new `src/git_history/git.rs`:

```rust
//! Read-only Git subprocesses: command setup, capped pipe drains, and a
//! watchdog that kills and reaps the child on cancel or timeout.
use std::io::Read;
use std::path::Path;
use std::process::{ChildStdout, Command, ExitStatus, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_history::tests::git;

    fn big_blob() -> (tempfile::TempDir, String) {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-b", "main"]);
        std::fs::write(repo.path().join("big.txt"), "x".repeat(4 << 20)).unwrap();
        let blob = git(repo.path(), &["hash-object", "-w", "big.txt"]);
        (repo, blob)
    }
    fn flag(v: bool) -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(v))
    }

    #[test]
    fn cancel_kills_a_child_blocked_on_a_full_pipe() {
        let (repo, blob) = big_blob();
        let cancel = flag(false);
        let (stdout, exit) = spawn(repo.path(), &["cat-file", "-p", &blob], &cancel, None).unwrap();
        // Nobody reads stdout, so git blocks once the pipe buffer fills.
        std::thread::sleep(Duration::from_millis(200));
        cancel.store(true, Ordering::Relaxed);
        let start = Instant::now();
        assert_eq!(exit.finish(), Err(GitError::Cancelled));
        assert!(start.elapsed() < Duration::from_secs(5));
        drop(stdout);
    }

    #[test]
    fn timeout_kills_a_blocked_child() {
        let (repo, blob) = big_blob();
        let (_stdout, exit) = spawn(
            repo.path(),
            &["cat-file", "-p", &blob],
            &flag(false),
            Some(Duration::from_millis(200)),
        )
        .unwrap();
        assert_eq!(exit.finish(), Err(GitError::TimedOut));
    }

    #[test]
    fn output_caps_bytes_and_reports_failures_with_stderr() {
        let (repo, blob) = big_blob();
        let read = |args: &[&str], cap| {
            output(repo.path(), args, &flag(false), cap, Duration::from_secs(30))
        };
        assert_eq!(read(&["cat-file", "-p", &blob], 1024), Err(GitError::TooLarge));
        assert_eq!(read(&["cat-file", "-p", &blob], 8 << 20).unwrap().len(), 4 << 20);
        match read(&["rev-parse", "--verify", "refs/heads/nope"], 1024) {
            Err(GitError::Failed(stderr)) => assert!(!stderr.is_empty()),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            spawn(repo.path(), &["status"], &flag(true), None).map(|_| ()),
            Err(GitError::Cancelled)
        );
    }
}
```

Add `mod git;` next to `mod details;` in `src/git_history.rs`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --target-dir target/agent git_history::git`
Expected: compile errors: `spawn`, `output`, `GitError` not found.

- [ ] **Step 3: Implement the helper** above the tests in `git.rs`:

```rust
const STDERR_CAP: usize = 16 * 1024;

#[derive(Debug, PartialEq)]
pub(super) enum GitError {
    Spawn(String),
    Cancelled,
    TimedOut,
    TooLarge,
    /// Non-zero exit; carries trimmed stderr (may be empty).
    Failed(String),
    Io(String),
}
impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "Cannot start Git: {e}"),
            Self::Cancelled => f.write_str("Git read cancelled"),
            Self::TimedOut => f.write_str("Git timed out"),
            Self::TooLarge => f.write_str("Git output is too large to display"),
            Self::Failed(e) | Self::Io(e) => f.write_str(e),
        }
    }
}

pub(super) struct Exit {
    stderr: JoinHandle<Vec<u8>>,
    status: mpsc::Receiver<Result<ExitStatus, GitError>>,
}
impl Exit {
    /// Wait for the watchdog's verdict. Callers must have drained or dropped
    /// stdout first, or set a timeout: a child blocked on a full pipe never exits.
    pub(super) fn finish(self) -> Result<(), GitError> {
        let status = self
            .status
            .recv()
            .map_err(|_| GitError::Io("Git watchdog stopped".into()))??;
        let stderr = self.stderr.join().unwrap_or_default();
        if status.success() {
            Ok(())
        } else {
            Err(GitError::Failed(
                String::from_utf8_lossy(&stderr).trim().to_owned(),
            ))
        }
    }
}

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

/// Start `git --no-pager <args>` in `cwd`. A watchdog thread owns the child:
/// it kills and reaps it when `cancel` is set or `timeout` passes, so a reader
/// blocked on stdout is released. The GUI thread never calls this.
pub(super) fn spawn(
    cwd: &Path,
    args: &[&str],
    cancel: &Arc<AtomicBool>,
    timeout: Option<Duration>,
) -> Result<(ChildStdout, Exit), GitError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(GitError::Cancelled);
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
    let mut child = cmd.spawn().map_err(|e| GitError::Spawn(e.to_string()))?;
    let stdout = child.stdout.take().unwrap();
    let pipe = child.stderr.take().unwrap();
    let stderr = std::thread::spawn(move || {
        drain(pipe, STDERR_CAP).map(|(b, _)| b).unwrap_or_default()
    });
    let (tx, status) = mpsc::channel();
    let stop = cancel.clone();
    let start = Instant::now();
    std::thread::spawn(move || {
        let outcome = loop {
            if stop.load(Ordering::Relaxed) {
                break Err(GitError::Cancelled);
            }
            if timeout.is_some_and(|t| start.elapsed() > t) {
                break Err(GitError::TimedOut);
            }
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(e) => break Err(GitError::Io(e.to_string())),
            }
        };
        if outcome.is_err() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = tx.send(outcome);
    });
    Ok((stdout, Exit { stderr, status }))
}

/// One-shot read: all of stdout (at most `cap` bytes kept), then the exit verdict.
pub(super) fn output(
    cwd: &Path,
    args: &[&str],
    cancel: &Arc<AtomicBool>,
    cap: usize,
    timeout: Duration,
) -> Result<Vec<u8>, GitError> {
    let (stdout, exit) = spawn(cwd, args, cancel, Some(timeout))?;
    let drained = drain(stdout, cap);
    exit.finish()?;
    let (bytes, overflow) = drained.map_err(|e| GitError::Io(e.to_string()))?;
    if overflow {
        return Err(GitError::TooLarge);
    }
    Ok(bytes)
}
```

- [ ] **Step 4: Run the new tests to verify they pass**

Run: `cargo test --target-dir target/agent git_history::git`
Expected: 3 passed.

- [ ] **Step 5: Convert `details.rs` to the helper.** Replace the whole body of `git_output` (keep its name so `load` is untouched apart from the signature):

```rust
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
```

Change `fn load(cwd: &Path, hash: &str, cancel: &AtomicBool)` to take `cancel: &Arc<AtomicBool>`. `DetailsView::select` already passes an `Arc`. In the `details.rs` tests, replace every `&AtomicBool::new(x)` with `&Arc::new(AtomicBool::new(x))` (in `read` and the three `load(...)` asserts). Remove the `Command`, `Stdio`, and `Instant` imports and add `use std::time::Duration;` if `super::*` no longer provides it.

- [ ] **Step 6: Convert `stream_history`** in `src/git_history.rs`. Replace everything from `use std::process::{Command, Stdio};` through the supervisor thread's closing `});` with:

```rust
    let (stdout, exit) = git::spawn(
        &cwd,
        &[
            "log",
            "--all",
            "--topo-order",
            "--decorate=short",
            "--no-color",
            "--no-patch",
            "--encoding=UTF-8",
            "--no-show-signature",
            "-z",
            "--format=%H%x00%P%x00%D%x00%an%x00%as%x00%s",
            "--",
        ],
        cancel,
        None,
    )
    .map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(stdout);
    let mut exit = Some(exit);
```

And replace the `let error = if end { ... }` block with:

```rust
            let error = if end {
                match exit.take().expect("the stream ends once").finish() {
                    Ok(()) => None,
                    Err(git::GitError::Failed(stderr)) if stderr.is_empty() => {
                        Some("Git history could not be read".into())
                    }
                    Err(e) => Some(e.to_string()),
                }
            } else {
                None
            };
```

Remove now-unused imports (`Duration` moves into `mod tests` if only tests use it). Run `cargo check --target-dir target/agent` and fix until there are no new warnings.

- [ ] **Step 7: Run the whole history suite** (the gate for "behavior-preserving")

Run: `cargo test --target-dir target/agent git_history`
Expected: all previous `git_history` and `details` tests plus the 3 new ones pass.

- [ ] **Step 8: Commit**

```bash
cargo fmt
git add src/git_history.rs src/git_history/details.rs src/git_history/git.rs
git commit -m "refactor(history): share one git subprocess helper" -m "Extract command setup, capped drains, and the kill/reap watchdog used by
the history stream and commit details, so diff reads do not add a third copy.
Errors are typed so callers can tell too-large from failed.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Card: l8th0t"
```

---

### Task 2: Pure diff parser and row model

**Files:**
- Create: `src/git_history/diff.rs`
- Modify: `src/git_history.rs` (add `mod diff;`)

**Interfaces:**
- Produces (all `pub(super)`):
  - `const CONTEXT_LINES: usize = 200_000;`
  - `enum Diff { Doc(Doc), Notice(Notice) }`, `enum Notice { Binary, TooLarge, Unchanged, Submodule }`
  - `enum Kind { Same, Removed, Added, Modified }` (`Clone, Copy, Debug, PartialEq`)
  - `struct Cell { line: u32, text: String, hot: Option<Range<usize>>, no_eol: bool }`
  - `struct Row { kind: Kind, old: Option<Cell>, new: Option<Cell> }`
  - `struct Block { rows: Range<usize>, kind: Kind }`: `kind` is `Modified` if the block has both removed and added lines, else `Removed` or `Added`
  - `struct Doc { rows: Vec<Row>, blocks: Vec<Block>, max_cols: usize, max_line: u32 }`
  - `fn parse(bytes: &[u8]) -> Result<Diff, String>`
  - `impl Notice { fn text(&self) -> &'static str }`

- [ ] **Step 1: Write the failing tests** at the bottom of the new `diff.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn doc(bytes: &str) -> Doc {
        match parse(bytes.as_bytes()).unwrap() {
            Diff::Doc(d) => d,
            other => panic!("{other:?}"),
        }
    }
    fn kinds(d: &Doc) -> Vec<Kind> {
        d.rows.iter().map(|r| r.kind).collect()
    }
    const HEAD: &str = "diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n";

    #[test]
    fn pairs_removed_runs_with_following_added_runs_and_pads() {
        use Kind::*;
        let d = doc(&format!(
            "{HEAD}@@ -1,6 +1,9 @@\n a\n-b\n-c\n-d\n+B\n+C\n+D\n+E\n+F\n+G\n e\n f\n"
        ));
        // 3 removed then 6 added: 6 rows, 3 Modified + 3 Added.
        assert_eq!(kinds(&d), [Same, Modified, Modified, Modified, Added, Added, Added, Same, Same]);
        assert_eq!(d.blocks, [Block { rows: 1..7, kind: Modified }]);
        assert!(d.rows[4].old.is_none());
        assert_eq!(d.rows[4].new.as_ref().unwrap().line, 5);
        assert_eq!(d.rows[7].old.as_ref().unwrap().line, 5);
        assert_eq!(d.rows[7].new.as_ref().unwrap().line, 8);
        assert_eq!(d.max_line, 9);

        let d = doc(&format!("{HEAD}@@ -1,4 +1,2 @@\n-a\n-b\n-c\n+A\n x\n"));
        assert_eq!(kinds(&d), [Modified, Removed, Removed, Same]);
        assert!(d.rows[1].new.is_none());
    }

    #[test]
    fn lone_runs_and_adjacent_blocks_are_separate_blocks() {
        use Kind::*;
        let d = doc(&format!("{HEAD}@@ -1,4 +1,4 @@\n+new\n a\n-gone\n b\n-x\n+y\n"));
        assert_eq!(kinds(&d), [Added, Same, Removed, Same, Modified]);
        assert_eq!(
            d.blocks,
            [
                Block { rows: 0..1, kind: Added },
                Block { rows: 2..3, kind: Removed },
                Block { rows: 4..5, kind: Modified },
            ]
        );
    }

    #[test]
    fn strict_single_hunk_rejects_partial_or_corrupt_output() {
        // Shape of git 2.39's -U<INT_MAX> overflow: repeated hunks from line 1.
        let twice = format!("{HEAD}@@ -1,1 +1,1 @@\n-a\n+b\n@@ -1,1 +1,2 @@\n-a\n+b\n+c\n");
        assert_eq!(parse(twice.as_bytes()).unwrap(), Diff::Notice(Notice::TooLarge));
        let short = format!("{HEAD}@@ -1,3 +1,3 @@\n a\n");
        assert_eq!(parse(short.as_bytes()).unwrap(), Diff::Notice(Notice::TooLarge));
        let late = format!("{HEAD}@@ -40,2 +40,2 @@\n-a\n+b\n c\n");
        assert_eq!(parse(late.as_bytes()).unwrap(), Diff::Notice(Notice::TooLarge));
        assert!(parse(format!("{HEAD}@@ nonsense @@\n").as_bytes()).is_err());
        assert!(parse(format!("{HEAD}@@ -1 +1 @@\n?what\n").as_bytes()).is_err());
    }

    #[test]
    fn notices_for_binary_submodule_and_no_content_change() {
        let bin = "diff --git a/i.png b/i.png\nindex 1..2 100644\nBinary files a/i.png and b/i.png differ\n";
        assert_eq!(parse(bin.as_bytes()).unwrap(), Diff::Notice(Notice::Binary));
        let sub = "diff --git a/s b/s\nindex 1111111..2222222 160000\n--- a/s\n+++ b/s\n@@ -1 +1 @@\n-Subproject commit 1111111\n+Subproject commit 2222222\n";
        assert_eq!(parse(sub.as_bytes()).unwrap(), Diff::Notice(Notice::Submodule));
        let new_sub = "diff --git a/s b/s\nnew file mode 160000\nindex 0000000..2222222\n--- /dev/null\n+++ b/s\n@@ -0,0 +1 @@\n+Subproject commit 2222222\n";
        assert_eq!(parse(new_sub.as_bytes()).unwrap(), Diff::Notice(Notice::Submodule));
        let empty_add = "diff --git a/e b/e\nnew file mode 100644\nindex 0000000..e69de29\n";
        assert_eq!(parse(empty_add.as_bytes()).unwrap(), Diff::Notice(Notice::Unchanged));
        assert_eq!(parse(b"").unwrap(), Diff::Notice(Notice::Unchanged));
    }

    #[test]
    fn added_and_deleted_files_start_at_zero() {
        use Kind::*;
        let d = doc("diff --git a/n b/n\nnew file mode 100644\n--- /dev/null\n+++ b/n\n@@ -0,0 +1,2 @@\n+one\n+two\n");
        assert_eq!(kinds(&d), [Added, Added]);
        assert_eq!(d.rows[1].new.as_ref().unwrap().line, 2);
        let d = doc("diff --git a/n b/n\ndeleted file mode 100644\n--- a/n\n+++ /dev/null\n@@ -1 +0,0 @@\n-one\n");
        assert_eq!(kinds(&d), [Removed]);
    }

    #[test]
    fn no_newline_marker_and_crlf() {
        use Kind::*;
        // Only the final newline changed: a Modified row with nothing hot.
        let d = doc(&format!("{HEAD}@@ -1,2 +1,2 @@\n a\n-end\n\\ No newline at end of file\n+end\n"));
        assert_eq!(kinds(&d), [Same, Modified]);
        let row = &d.rows[1];
        assert!(row.old.as_ref().unwrap().no_eol);
        assert!(!row.new.as_ref().unwrap().no_eol);
        assert_eq!(row.old.as_ref().unwrap().hot, None);
        assert_eq!(row.new.as_ref().unwrap().hot, None);
        // CRLF is displayed as LF; a pure line-ending change still shows as Modified.
        let d = doc(&format!("{HEAD}@@ -1 +1 @@\n-same\r\n+same\n"));
        assert_eq!(kinds(&d), [Modified]);
        assert_eq!(d.rows[0].old.as_ref().unwrap().text, "same");
        // An empty context line (diff.suppressBlankEmpty) is still a context line.
        let d = doc(&format!("{HEAD}@@ -1,3 +1,3 @@\n a\n\n-b\n+c\n"));
        assert_eq!(kinds(&d), [Same, Same, Modified]);
    }

    #[test]
    fn tabs_expand_to_four_column_stops() {
        let d = doc(&format!("{HEAD}@@ -1 +1 @@\n-a\tb\n+\tab\tc\n"));
        assert_eq!(d.rows[0].old.as_ref().unwrap().text, "a   b");
        assert_eq!(d.rows[0].new.as_ref().unwrap().text, "    ab  c");
        assert_eq!(d.max_cols, 9);
    }

    #[test]
    fn trim_spans_multibyte_chars() {
        let d = doc(&format!("{HEAD}@@ -1 +1 @@\n-let café = 1;\n+let caféé = 12;\n"));
        let (old, new) = (d.rows[0].old.as_ref().unwrap(), d.rows[0].new.as_ref().unwrap());
        let hot = |c: &Cell| &c.text[c.hot.clone().unwrap()];
        assert_eq!(hot(old), " = 1");
        assert_eq!(hot(new), "é = 12");
        // Pure insertion inside a line: only the new side is hot.
        let d = doc(&format!("{HEAD}@@ -1 +1 @@\n-ab\n+aXb\n"));
        assert_eq!(d.rows[0].old.as_ref().unwrap().hot, None);
        assert_eq!(d.rows[0].new.as_ref().unwrap().hot, Some(1..2));
    }
}
```

Note on the `café` case: the prefix is `let caf` + `é`, since both lines share `café`. The suffix search runs on what's left after the prefix (`" = 1;"` vs `"é = 12;"`) and matches only `;`, which leaves `" = 1"` and `"é = 12"` hot. The test pins exactly that.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --target-dir target/agent git_history::diff`
Expected: compile errors (types and `parse` missing).

- [ ] **Step 3: Implement** above the tests:

```rust
//! Pure unified-diff parsing into aligned side-by-side rows. No egui, no I/O:
//! everything the painter needs is computed here, once, on the worker.
use std::ops::Range;

/// `-U` value for diff reads, and so the effective line cap: a change more than
/// this many lines from the file start, or from the next change, cannot come
/// back as one whole-file hunk, which `parse` reports as `Notice::TooLarge`.
/// Must stay far below i32::MAX: git 2.39 emits overlapping hunks at `-U2147483647`.
pub(super) const CONTEXT_LINES: usize = 200_000;

#[derive(Debug, PartialEq)]
pub(super) enum Notice {
    Binary,
    TooLarge,
    Unchanged,
    Submodule,
}
impl Notice {
    pub(super) fn text(&self) -> &'static str {
        match self {
            Notice::Binary => "Binary file — not shown.",
            Notice::TooLarge => "Diff too large to show (over 16 MiB, or changes over 200,000 lines apart).",
            Notice::Unchanged => "No content changes.",
            Notice::Submodule => "Submodule change — not shown.",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Kind {
    Same,
    Removed,
    Added,
    Modified,
}
#[derive(Debug, PartialEq)]
pub(super) struct Cell {
    pub(super) line: u32,
    /// Display text: lossy UTF-8, trailing `\r` stripped, tabs expanded.
    pub(super) text: String,
    /// Byte range (char-aligned) of the differing middle, `Modified` rows only.
    pub(super) hot: Option<Range<usize>>,
    pub(super) no_eol: bool,
}
#[derive(Debug, PartialEq)]
pub(super) struct Row {
    pub(super) kind: Kind,
    pub(super) old: Option<Cell>,
    pub(super) new: Option<Cell>,
}
#[derive(Debug, PartialEq)]
pub(super) struct Block {
    pub(super) rows: Range<usize>,
    pub(super) kind: Kind,
}
#[derive(Debug, Default, PartialEq)]
pub(super) struct Doc {
    pub(super) rows: Vec<Row>,
    pub(super) blocks: Vec<Block>,
    pub(super) max_cols: usize,
    pub(super) max_line: u32,
}
#[derive(Debug, PartialEq)]
pub(super) enum Diff {
    Doc(Doc),
    Notice(Notice),
}

const MALFORMED: &str = "Git returned a malformed diff";

pub(super) fn parse(bytes: &[u8]) -> Result<Diff, String> {
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    let mut lines = bytes.split(|b| *b == b'\n');
    let mut header = None;
    for line in lines.by_ref() {
        if line.starts_with(b"@@") {
            header = Some(parse_header(line)?);
            break;
        }
        if line.starts_with(b"Binary files ") || line.starts_with(b"GIT binary patch") {
            return Ok(Diff::Notice(Notice::Binary));
        }
        let gitlink = (line.starts_with(b"index ") && line.ends_with(b" 160000"))
            || line.ends_with(b"file mode 160000");
        if gitlink {
            return Ok(Diff::Notice(Notice::Submodule));
        }
    }
    let Some((old_start, old_count, new_start, new_count)) = header else {
        return Ok(Diff::Notice(Notice::Unchanged));
    };
    if old_start > 1 || new_start > 1 {
        return Ok(Diff::Notice(Notice::TooLarge));
    }
    let mut b = Builder::default();
    for line in lines {
        match line.first() {
            Some(b' ') => b.context(&line[1..]),
            None => b.context(b""),
            Some(b'-') => b.removed(&line[1..]),
            Some(b'+') => b.added(&line[1..]),
            Some(b'\\') => b.no_eol(),
            Some(b'@') => return Ok(Diff::Notice(Notice::TooLarge)),
            _ => return Err(MALFORMED.into()),
        }
    }
    b.flush();
    if b.old_no != old_count || b.new_no != new_count {
        return Ok(Diff::Notice(Notice::TooLarge));
    }
    b.doc.max_line = b.old_no.max(b.new_no);
    Ok(Diff::Doc(b.doc))
}

/// `@@ -a[,b] +c[,d] @@[ context]` → (a, b, c, d); a missing count means 1.
fn parse_header(line: &[u8]) -> Result<(u32, u32, u32, u32), String> {
    let text = String::from_utf8_lossy(line);
    let mut parts = text.split(' ');
    let (Some("@@"), Some(old), Some(new), Some("@@")) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(MALFORMED.into());
    };
    let range = |s: &str, sign: char| -> Option<(u32, u32)> {
        let s = s.strip_prefix(sign)?;
        match s.split_once(',') {
            Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
            None => Some((s.parse().ok()?, 1)),
        }
    };
    let ((a, b), (c, d)) = range(old, '-')
        .zip(range(new, '+'))
        .ok_or(MALFORMED)?;
    Ok((a, b, c, d))
}

#[derive(Clone, Copy, Default, PartialEq)]
enum Last {
    #[default]
    None,
    Context,
    Removed,
    Added,
}
#[derive(Default)]
struct Builder {
    doc: Doc,
    /// Lines consumed so far on each side = the last line number used.
    old_no: u32,
    new_no: u32,
    removed: Vec<Cell>,
    added: Vec<Cell>,
    last: Last,
}
impl Builder {
    fn cell(&mut self, line: u32, raw: &[u8]) -> Cell {
        let (text, cols) = display(raw);
        self.doc.max_cols = self.doc.max_cols.max(cols);
        Cell { line, text, hot: None, no_eol: false }
    }
    fn context(&mut self, raw: &[u8]) {
        self.flush();
        self.old_no += 1;
        self.new_no += 1;
        let old = self.cell(self.old_no, raw);
        let new = self.cell(self.new_no, raw);
        self.doc.rows.push(Row { kind: Kind::Same, old: Some(old), new: Some(new) });
        self.last = Last::Context;
    }
    fn removed(&mut self, raw: &[u8]) {
        // A '-' after '+' lines starts a new block.
        if !self.added.is_empty() {
            self.flush();
        }
        self.old_no += 1;
        let cell = self.cell(self.old_no, raw);
        self.removed.push(cell);
        self.last = Last::Removed;
    }
    fn added(&mut self, raw: &[u8]) {
        self.new_no += 1;
        let cell = self.cell(self.new_no, raw);
        self.added.push(cell);
        self.last = Last::Added;
    }
    fn no_eol(&mut self) {
        match self.last {
            Last::Removed => self.removed.last_mut().map(|c| c.no_eol = true),
            Last::Added => self.added.last_mut().map(|c| c.no_eol = true),
            Last::Context => self.doc.rows.last_mut().map(|r| {
                for c in [&mut r.old, &mut r.new].into_iter().flatten() {
                    c.no_eol = true;
                }
            }),
            Last::None => None,
        };
    }
    fn flush(&mut self) {
        if self.removed.is_empty() && self.added.is_empty() {
            return;
        }
        let kind = match (self.removed.is_empty(), self.added.is_empty()) {
            (false, false) => Kind::Modified,
            (false, true) => Kind::Removed,
            _ => Kind::Added,
        };
        let start = self.doc.rows.len();
        let n = self.removed.len().max(self.added.len());
        let mut old = std::mem::take(&mut self.removed).into_iter();
        let mut new = std::mem::take(&mut self.added).into_iter();
        for _ in 0..n {
            let (mut o, mut a) = (old.next(), new.next());
            let kind = match (&o, &a) {
                (Some(_), Some(_)) => Kind::Modified,
                (Some(_), None) => Kind::Removed,
                _ => Kind::Added,
            };
            if let (Some(o), Some(a)) = (&mut o, &mut a) {
                trim(o, a);
            }
            self.doc.rows.push(Row { kind, old: o, new: a });
        }
        self.doc.blocks.push(Block { rows: start..self.doc.rows.len(), kind });
    }
}

/// Lossy-decode, strip one trailing `\r`, expand tabs to 4-column stops.
/// Returns the text and its width in columns (chars).
fn display(raw: &[u8]) -> (String, usize) {
    let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
    let text = String::from_utf8_lossy(raw);
    let mut out = String::with_capacity(text.len());
    let mut col = 0;
    for c in text.chars() {
        if c == '\t' {
            let n = 4 - col % 4;
            out.extend(std::iter::repeat_n(' ', n));
            col += n;
        } else {
            out.push(c);
            col += 1;
        }
    }
    (out, col)
}

/// Mark the differing middle of a paired line, excluding the common
/// char-aligned prefix and suffix. An empty middle stays `None`.
fn trim(old: &mut Cell, new: &mut Cell) {
    let (a, b) = (old.text.as_str(), new.text.as_str());
    let prefix: usize = a
        .chars()
        .zip(b.chars())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x.len_utf8())
        .sum();
    let suffix: usize = a[prefix..]
        .chars()
        .rev()
        .zip(b[prefix..].chars().rev())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x.len_utf8())
        .sum();
    let span = |len: usize| (prefix < len - suffix).then(|| prefix..len - suffix);
    let (ol, nl) = (a.len(), b.len());
    old.hot = span(ol);
    new.hot = span(nl);
}
```

Add `mod diff;` in `src/git_history.rs`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --target-dir target/agent git_history::diff`
Expected: 8 passed. If `trim_spans_multibyte_chars` fails, check the trim, not the test: the expected spans are worked out in the note under Step 1.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add src/git_history.rs src/git_history/diff.rs
git commit -m "feat(history): parse whole-file diffs into side-by-side rows" -m "Pair removed/added runs into padded rows and blocks, trim word-level
changes, and reject anything but one whole-file hunk so a partial or corrupt
git output becomes a notice instead of a wrong picture.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Card: l8th0t"
```

---

### Task 3: Diff targets and git reads

**Files:**
- Create: `src/git_history/diff_view.rs` (target + load half; the view comes in Task 4)
- Modify: `src/git_history.rs` (add `mod diff_view; pub use diff_view::DiffTarget;`)
- Modify: `src/git_history/details.rs` (make `display_path` and `status_color` `pub(super)`)

**Interfaces:**
- Consumes: `git::output`, `git::GitError` (Task 1); `diff::{parse, Diff, Notice, CONTEXT_LINES}` (Task 2)
- Produces:
  - `pub struct DiffTarget { pub commit: String, pub parent: Option<String>, pub status: char, pub old_path: Option<String>, pub path: String, pub merge: bool }` (`Clone, Debug, PartialEq`)
  - `impl DiffTarget { pub fn file_name(&self) -> &str; fn versus(&self) -> String; fn args(&self) -> Result<Vec<String>, String> }`
  - `pub(super) fn load(cwd: &Path, target: &DiffTarget, cancel: &Arc<AtomicBool>) -> Result<Diff, String>`

- [ ] **Step 1: Write the failing tests** in the new `src/git_history/diff_view.rs`:

```rust
//! The Diff window: a reusable per-Project side-by-side view of one file's
//! change at one commit, loaded on a cancellable worker.
use super::diff::{self, Diff, Notice};
use super::git;
use std::path::Path;
use std::sync::{Arc, atomic::AtomicBool};
use std::time::Duration;

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
        let m = lines(1000).replace("line 10\n", "line ten\n").replace("line 900\n", "line nine hundred\n");
        std::fs::write(dir.join("m.txt"), m).unwrap();
        std::fs::remove_file(dir.join("d.txt")).unwrap();
        std::fs::write(dir.join("a.txt"), "new\nfile\n").unwrap();
        git(dir, &["mv", "old.txt", "new.txt"]);
        std::fs::write(dir.join("new.txt"), lines(50).replace("line 25\n", "line 25!\n")).unwrap();
        git(dir, &["mv", "same.txt", "same2.txt"]);
        git(dir, &["add", "-A", "."]);
        // Type change without a real symlink: stage mode 120000 directly.
        std::fs::write(dir.join("link.tmp"), "link-target").unwrap();
        let blob = git(dir, &["hash-object", "-w", "link.tmp"]);
        std::fs::remove_file(dir.join("link.tmp")).unwrap();
        git(dir, &["update-index", "--cacheinfo", &format!("120000,{blob},t.txt")]);
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
        let r = doc(load(dir, &target(dir, 'R', Some("old.txt"), "new.txt"), &flag()));
        assert_eq!(r.blocks.len(), 1);
        assert_eq!(r.blocks[0].rows, 24..25);
        let c = doc(load(dir, &target(dir, 'C', Some("old.txt"), "new.txt"), &flag()));
        assert_eq!(c.blocks, r.blocks);
        assert_eq!(
            load(dir, &target(dir, 'R', Some("same.txt"), "same2.txt"), &flag()).unwrap(),
            Diff::Notice(Notice::Unchanged)
        );
        let t = doc(load(dir, &target(dir, 'T', None, "t.txt"), &flag()));
        assert_eq!(t.blocks.len(), 1);
        assert_eq!(git(dir, &["status", "--porcelain=v1"]), before, "diff reads must not write");
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
        let added = DiffTarget { status: 'A', parent: None, merge: false, ..t };
        assert_eq!(added.versus(), "abcdef0 · new file");
    }
}
```

Add to `src/git_history.rs`: `mod diff_view;` and `pub use diff_view::DiffTarget;`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --target-dir target/agent git_history::diff_view`
Expected: compile errors (`DiffTarget`, `load` missing).

- [ ] **Step 3: Implement** above the tests:

```rust
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
    /// pairs renamed/copied paths exactly and keeps a type change one hunk.
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
                args.extend([parent.clone(), self.commit.clone(), "--".into(), self.path.clone()]);
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
pub(super) fn load(cwd: &Path, target: &DiffTarget, cancel: &Arc<AtomicBool>) -> Result<Diff, String> {
    let args = target.args()?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match git::output(cwd, &args, cancel, 16 << 20, Duration::from_secs(30)) {
        Ok(bytes) => diff::parse(&bytes),
        Err(git::GitError::TooLarge) => Ok(Diff::Notice(Notice::TooLarge)),
        Err(git::GitError::Failed(stderr)) if stderr.is_empty() => {
            Err("Git could not read this diff".into())
        }
        Err(e) => Err(e.to_string()),
    }
}
```

In `details.rs`, change `fn display_path` and `fn status_color` to `pub(super) fn`. They're used in Task 4.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --target-dir target/agent git_history::diff_view`
Expected: 4 passed. The line-cap test writes about 2.4 MB, and a few seconds in debug is fine. Dead-code warnings from `diff_view.rs` are expected until Task 4 uses it; there should be none from elsewhere.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add src/git_history.rs src/git_history/details.rs src/git_history/diff_view.rs
git commit -m "feat(history): read one file's diff for any change status" -m "diff-tree for adds, deletes and modifications (submodules keep their
gitlink header); the blob form for renames, copies and type changes. Bounded
-U doubles as the line cap.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Card: l8th0t"
```

---

### Task 4: `DiffView`: request lifecycle, painting, navigation

**Files:**
- Modify: `src/git_history/diff_view.rs`
- Modify: `src/git_history.rs` (`pub use diff_view::{DiffTarget, DiffView};`)

**Interfaces:**
- Consumes: `DiffTarget`, `load` (Task 3); `diff::{Doc, Row, Cell, Kind, Block}` (Task 2); `details::{display_path, status_color}` (Task 3 made them `pub(super)`)
- Produces:
  - `pub struct DiffView` with `pub fn new(cwd: Option<PathBuf>) -> Self`, `pub fn retarget(&mut self, target: DiffTarget)`, `pub fn target(&self) -> Option<&DiffTarget>`, `pub fn show(&mut self, ui: &mut egui::Ui, rect: egui::Rect, active: bool, base: egui::Id)`
  - `fn step_block(blocks: &[Block], current: Option<usize>, anchor: usize, forward: bool) -> Option<usize>` (pure)
  - `fn visible(text: &str, skip: usize, take: usize) -> Range<usize>` (pure)

- [ ] **Step 1: Write the failing tests.** Append to the `tests` module in `diff_view.rs`:

```rust
    fn synthetic(n: usize, changed: &[usize]) -> diff::Doc {
        let mut s = format!("diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,{n} +1,{n} @@\n");
        for i in 1..=n {
            if changed.contains(&i) {
                s += &format!("-line {i}\n+LINE {i}\n");
            } else {
                s += &format!(" line {i}\n");
            }
        }
        match diff::parse(s.as_bytes()).unwrap() {
            Diff::Doc(d) => d,
            other => panic!("{other:?}"),
        }
    }
    fn loaded(doc: diff::Doc) -> DiffView {
        let mut view = DiffView::new(None);
        view.target = Some(DiffTarget {
            commit: "a".repeat(40),
            parent: Some("b".repeat(40)),
            status: 'M',
            old_path: None,
            path: "f".into(),
            merge: false,
        });
        view.result = Some(Ok(Diff::Doc(doc)));
        view.nav.first = true; // what retarget would set
        view
    }
    fn run(ctx: &egui::Context, view: &mut DiffView, events: Vec<egui::Event>) {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 600.0));
        let _ = ctx.run_ui(
            egui::RawInput { screen_rect: Some(rect), events, ..Default::default() },
            |ui| view.show(ui, rect, true, egui::Id::new("diff")),
        );
    }
    fn key(key: egui::Key, shift: bool) -> Vec<egui::Event> {
        let modifiers = if shift { egui::Modifiers::SHIFT } else { egui::Modifiers::NONE };
        vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }]
    }

    #[test]
    fn step_block_follows_current_then_falls_back_to_anchor() {
        let b = |r: std::ops::Range<usize>| diff::Block { rows: r, kind: diff::Kind::Modified };
        let blocks = [b(2..3), b(10..12), b(40..41)];
        assert_eq!(step_block(&blocks, Some(0), 99, true), Some(1));
        assert_eq!(step_block(&blocks, Some(2), 0, true), None);
        assert_eq!(step_block(&blocks, Some(1), 0, false), Some(0));
        assert_eq!(step_block(&blocks, Some(0), 0, false), None);
        // Free scroll: anchor row decides.
        assert_eq!(step_block(&blocks, None, 10, true), Some(2));
        assert_eq!(step_block(&blocks, None, 10, false), Some(0));
        assert_eq!(step_block(&blocks, None, 0, true), Some(0));
        assert_eq!(step_block(&[], None, 0, true), None);
    }

    #[test]
    fn visible_slice_respects_char_boundaries() {
        let s = "héllo wörld";
        assert_eq!(&s[visible(s, 1, 2)], "él");
        assert_eq!(&s[visible(s, 7, 100)], "örld");
        assert_eq!(visible(s, 100, 5), s.len()..s.len());
        assert_eq!(visible(s, 2, 0), 3..3);
        assert_eq!(&s[visible("日本語", 1, 1)], "本");
    }

    #[test]
    fn visible_slice_bounds_huge_lines() {
        let line = "x".repeat(1_000_000);
        let r = visible(&line, 500_000, 120);
        assert_eq!(r.len(), 120);
    }

    #[test]
    fn opens_on_first_difference_and_f7_walks_blocks() {
        let ctx = egui::Context::default();
        let mut view = loaded(synthetic(5000, &[100, 2000, 4000]));
        run(&ctx, &mut view, vec![]);
        run(&ctx, &mut view, vec![]);
        assert!(view.drawn.contains(&99), "{:?}", view.drawn);
        run(&ctx, &mut view, key(egui::Key::F7, false));
        run(&ctx, &mut view, vec![]);
        assert!(view.drawn.contains(&1999), "{:?}", view.drawn);
        run(&ctx, &mut view, key(egui::Key::F7, false));
        run(&ctx, &mut view, vec![]);
        assert!(view.drawn.contains(&3999), "{:?}", view.drawn);
        run(&ctx, &mut view, key(egui::Key::F7, true));
        run(&ctx, &mut view, vec![]);
        assert!(view.drawn.contains(&1999), "{:?}", view.drawn);
        run(&ctx, &mut view, key(egui::Key::Home, false));
        run(&ctx, &mut view, vec![]);
        assert_eq!(view.drawn.start, 0);
    }

    #[test]
    fn large_diff_paints_only_viewport_rows() {
        let ctx = egui::Context::default();
        let start = std::time::Instant::now();
        let mut view = loaded(synthetic(100_000, &[50_000]));
        let parse_time = start.elapsed();
        let start = std::time::Instant::now();
        for _ in 0..12 {
            run(&ctx, &mut view, vec![]);
            assert!(view.drawn.len() < 60, "{:?}", view.drawn);
        }
        assert!(view.drawn.contains(&49_999), "{:?}", view.drawn);
        eprintln!("100k-line parse {parse_time:?}; 12 headless debug frames {:?}", start.elapsed());
    }

    #[test]
    fn rows_follow_theme_font_size() {
        let ctx = egui::Context::default();
        let mut view = loaded(synthetic(2000, &[1]));
        let mut rows_at = |px: f32| {
            crate::terminal::set_font_size(&ctx, px);
            for _ in 0..2 {
                run(&ctx, &mut view, vec![]);
            }
            view.drawn.len()
        };
        let base = rows_at(crate::config::DEFAULT_FONT_SIZE);
        let zoomed = rows_at(crate::config::DEFAULT_FONT_SIZE * 2.0);
        assert!(zoomed * 2 <= base + 2, "base {base}, zoomed {zoomed}");
    }

    #[test]
    fn retarget_cancels_the_previous_request() {
        let mut view = DiffView::new(Some(PathBuf::new()));
        let (_tx, receiver) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        view.target = Some(DiffTarget {
            commit: "a".repeat(40),
            parent: None,
            status: 'A',
            old_path: None,
            path: "old".into(),
            merge: false,
        });
        view.request = Some(Request { cancel: cancel.clone(), receiver });
        let mut next = view.target.clone().unwrap();
        next.path = "new".into();
        view.retarget(next.clone());
        assert!(cancel.load(Ordering::Relaxed));
        assert!(view.request.is_none() && view.result.is_none());
        assert_eq!(view.target(), Some(&next));
        // Same target again is a no-op (no restart) unless the last load failed.
        view.retarget(next.clone());
        assert_eq!(view.nav.generation, 1);
        view.result = Some(Err("boom".into()));
        view.retarget(next);
        assert!(view.result.is_none());
        assert_eq!(view.nav.generation, 2);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --target-dir target/agent git_history::diff_view`
Expected: compile errors (`DiffView`, `step_block`, `visible`, `Request` missing).

- [ ] **Step 3: Implement the view.** Replace the imports at the top of `diff_view.rs` with:

```rust
use super::details::{display_path, status_color};
use super::diff::{self, Block, Diff, Doc, Kind, Notice, Row};
use super::git;
use eframe::egui;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;
```

Then add below `load`:

```rust
const ROW_H: f32 = 18.0;
const GUTTER_W: f32 = 14.0;
const STRIP_W: f32 = 8.0;
const HBAR_H: f32 = 6.0;

struct Request {
    cancel: Arc<AtomicBool>,
    receiver: mpsc::Receiver<Result<Diff, String>>,
}
impl Drop for Request {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Scroll and navigation state; reset on every retarget.
#[derive(Default)]
struct Nav {
    /// Salts the ScrollArea id so a new target starts at the top.
    generation: u64,
    /// Jump to the first difference once the document arrives.
    first: bool,
    scroll_y: f32,
    hx: f32,
    /// Block the last prev/next landed on, valid while the offset is still
    /// `jumped_to`; any other scroll re-derives from the anchor row.
    current: Option<usize>,
    jumped_to: Option<f32>,
}

pub struct DiffView {
    cwd: Option<PathBuf>,
    target: Option<DiffTarget>,
    request: Option<Request>,
    result: Option<Result<Diff, String>>,
    nav: Nav,
    scale: f32,
    #[cfg(test)]
    drawn: Range<usize>,
}
impl Drop for DiffView {
    fn drop(&mut self) {
        self.clear();
    }
}

/// Next/previous block. While `current` is valid it steps from there;
/// otherwise the first block past (or last block before) the anchor row.
fn step_block(blocks: &[Block], current: Option<usize>, anchor: usize, forward: bool) -> Option<usize> {
    match (current, forward) {
        (Some(c), true) => (c + 1 < blocks.len()).then_some(c + 1),
        (Some(c), false) => c.checked_sub(1),
        (None, true) => blocks.iter().position(|b| b.rows.start > anchor),
        (None, false) => blocks.iter().rposition(|b| b.rows.start < anchor),
    }
}

/// Byte range of chars `skip..skip + take` of `text`, on char boundaries.
/// Bounds per-frame layout to what fits in the column, however long the line.
fn visible(text: &str, skip: usize, take: usize) -> Range<usize> {
    let mut ends = text.char_indices().map(|(i, _)| i).chain(std::iter::once(text.len()));
    let start = ends.nth(skip).unwrap_or(text.len());
    if take == 0 {
        return start..start;
    }
    let end = ends.nth(take - 1).unwrap_or(text.len());
    start..end
}

impl DiffView {
    pub fn new(cwd: Option<PathBuf>) -> Self {
        Self {
            cwd,
            target: None,
            request: None,
            result: None,
            nav: Nav::default(),
            scale: 1.0,
            #[cfg(test)]
            drawn: 0..0,
        }
    }
    pub fn target(&self) -> Option<&DiffTarget> {
        self.target.as_ref()
    }
    /// Show `target`. The load starts lazily on the next `show`, so restore can
    /// call this without an egui context.
    pub fn retarget(&mut self, target: DiffTarget) {
        let failed = matches!(self.result, Some(Err(_)));
        if self.target.as_ref() == Some(&target) && !failed {
            return;
        }
        self.clear();
        self.target = Some(target);
        self.nav = Nav { generation: self.nav.generation + 1, first: true, ..Nav::default() };
    }
    /// Cancel any read and free a possibly large document off the GUI thread.
    fn clear(&mut self) {
        let request = self.request.take();
        // Cancel now, not when the background drop runs: the worker must stop
        // before a new request starts.
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
    fn poll(&mut self, ctx: &egui::Context) {
        if self.result.is_some() {
            return;
        }
        let Some(target) = &self.target else { return };
        let Some(request) = &self.request else {
            let Some(cwd) = self.cwd.clone() else {
                self.result = Some(Err("This project has no directory".into()));
                return;
            };
            let (tx, receiver) = mpsc::sync_channel(1);
            let cancel = Arc::new(AtomicBool::new(false));
            let stop = cancel.clone();
            let target = target.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let result = load(&cwd, &target, &stop);
                if !stop.load(Ordering::Relaxed) {
                    let _ = tx.send(result);
                    ctx.request_repaint();
                }
            });
            self.request = Some(Request { cancel, receiver });
            return;
        };
        match request.receiver.try_recv() {
            Ok(result) => self.result = Some(result),
            Err(mpsc::TryRecvError::Disconnected) => {
                self.result = Some(Err("Diff worker stopped".into()))
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, rect: egui::Rect, active: bool, base: egui::Id) {
        self.poll(ui.ctx());
        let th = crate::theme::live(ui.ctx());
        let s = crate::terminal::font_size(ui.ctx()) / crate::config::DEFAULT_FONT_SIZE;
        let rescale = (s != self.scale).then(|| self.nav.scroll_y * s / self.scale);
        self.scale = s;
        ui.painter().rect_filled(rect, 0.0, th.bg);
        let mut ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(base)
                .max_rect(rect.shrink(8.0 * s))
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        ui.set_clip_rect(rect.intersect(ui.clip_rect()));
        ui.spacing_mut().button_padding *= s;
        ui.spacing_mut().interact_size *= s;
        for font in ui.style_mut().text_styles.values_mut() {
            font.size *= s;
        }
        let Some(target) = &self.target else {
            ui.colored_label(th.dim, "Select a file in Git History.");
            return;
        };
        let doc = match &self.result {
            None => {
                header(&mut ui, target, None, &th);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Loading diff…");
                });
                return;
            }
            Some(Err(error)) => {
                header(&mut ui, target, None, &th);
                ui.colored_label(th.dim, error.as_str());
                return;
            }
            Some(Ok(Diff::Notice(notice))) => {
                header(&mut ui, target, None, &th);
                ui.colored_label(th.dim, notice.text());
                return;
            }
            Some(Ok(Diff::Doc(doc))) => doc,
        };
        let step = header(&mut ui, target, Some(doc.blocks.len()), &th);
        #[cfg(test)]
        let drawn = &mut self.drawn;
        #[cfg(not(test))]
        let drawn = &mut (0..0);
        body(&mut ui, doc, &mut self.nav, active, step, s, rescale, base, &th, drawn);
    }
}

/// Path, versus label, difference count and prev/next. Returns a button step.
fn header(ui: &mut egui::Ui, target: &DiffTarget, blocks: Option<usize>, th: &crate::theme::Theme) -> Option<bool> {
    let mut step = None;
    ui.horizontal(|ui| {
        let path = match &target.old_path {
            Some(old) if old != &target.path => {
                format!("{} → {}", display_path(old), display_path(&target.path))
            }
            _ => display_path(&target.path),
        };
        ui.label(egui::RichText::new(path).color(th.text).strong());
        ui.label(egui::RichText::new(target.versus()).color(th.dim));
        if let Some(n) = blocks {
            let count = if n == 1 { "1 difference".to_owned() } else { format!("{n} differences") };
            ui.label(egui::RichText::new(count).color(th.dim));
            if ui.add_enabled(n > 0, egui::Button::new("▲")).on_hover_text("Previous difference (Shift+F7)").clicked() {
                step = Some(false);
            }
            if ui.add_enabled(n > 0, egui::Button::new("▼")).on_hover_text("Next difference (F7)").clicked() {
                step = Some(true);
            }
        }
    });
    step
}

fn kind_color(kind: Kind) -> egui::Color32 {
    match kind {
        Kind::Removed => status_color('D'),
        Kind::Added => status_color('A'),
        Kind::Modified | Kind::Same => status_color('M'),
    }
}

/// Column x-offsets from a row's left edge.
struct Cols {
    num_w: f32,
    side_w: f32,
    gutter_w: f32,
    char_w: f32,
}
impl Cols {
    fn old_num(&self) -> f32 { 0.0 }
    fn old_text(&self) -> f32 { self.num_w }
    fn gutter(&self) -> f32 { self.num_w + self.side_w }
    fn new_num(&self) -> f32 { self.gutter() + self.gutter_w }
    fn new_text(&self) -> f32 { self.new_num() + self.num_w }
}

#[allow(clippy::too_many_arguments)]
fn body(
    ui: &mut egui::Ui,
    doc: &Doc,
    nav: &mut Nav,
    active: bool,
    mut step: Option<bool>,
    s: f32,
    rescale: Option<f32>,
    base: egui::Id,
    th: &crate::theme::Theme,
    drawn: &mut Range<usize>,
) {
    let row_h = ROW_H * s;
    let font = egui::FontId::monospace(13.0 * s);
    let char_w = ui
        .ctx()
        .fonts_mut(|f| f.layout_no_wrap("0".into(), font.clone(), egui::Color32::WHITE))
        .size()
        .x;
    let area = ui.available_rect_before_wrap();
    let strip = egui::Rect::from_min_max(egui::pos2(area.right() - STRIP_W * s, area.top()), area.max);
    let main = egui::Rect::from_min_max(area.min, egui::pos2(strip.left() - 2.0 * s, area.bottom() - HBAR_H * s));
    let digits = doc.max_line.max(1).to_string().len() as f32;
    let num_w = (digits + 1.5) * char_w;
    let gutter_w = GUTTER_W * s;
    // Reserve the ScrollArea's vertical bar so the new side is not covered.
    let bar = ui.spacing().scroll.bar_width + ui.spacing().scroll.bar_outer_margin;
    let side_w = ((main.width() - bar - gutter_w) / 2.0 - num_w).max(char_w);
    let cols = Cols { num_w, side_w, gutter_w, char_w };
    let text_w = (doc.max_cols as f32 + 1.0) * char_w;
    let max_hx = (text_w - side_w).max(0.0);
    let view_h = main.height();

    if ui.rect_contains_pointer(main) {
        // egui already maps Shift+wheel to horizontal delta.
        nav.hx -= ui.input(|i| i.smooth_scroll_delta.x);
    }
    let mut target_y = rescale;
    if active {
        let page = (view_h / row_h).floor().max(1.0) * row_h;
        ui.input(|i| {
            let delta = if i.key_pressed(egui::Key::ArrowDown) {
                Some(row_h)
            } else if i.key_pressed(egui::Key::ArrowUp) {
                Some(-row_h)
            } else if i.key_pressed(egui::Key::PageDown) {
                Some(page)
            } else if i.key_pressed(egui::Key::PageUp) {
                Some(-page)
            } else {
                None
            };
            if let Some(d) = delta {
                target_y = Some(nav.scroll_y + d);
            }
            if i.key_pressed(egui::Key::Home) {
                target_y = Some(0.0);
            }
            if i.key_pressed(egui::Key::End) {
                target_y = Some(doc.rows.len() as f32 * row_h);
            }
            if i.key_pressed(egui::Key::F7) {
                step = Some(!i.modifiers.shift);
            }
        });
    }
    if nav.jumped_to.is_none_or(|y| (y - nav.scroll_y).abs() > 0.5) {
        nav.current = None;
    }
    let mut jumped = false;
    if std::mem::take(&mut nav.first) && !doc.blocks.is_empty() {
        nav.current = Some(0);
        target_y = Some(doc.blocks[0].rows.start as f32 * row_h - view_h / 3.0);
        jumped = true;
    } else if let Some(forward) = step {
        let anchor = ((nav.scroll_y + view_h / 3.0) / row_h).round() as usize;
        if let Some(k) = step_block(&doc.blocks, nav.current, anchor, forward) {
            nav.current = Some(k);
            target_y = Some(doc.blocks[k].rows.start as f32 * row_h - view_h / 3.0);
            jumped = true;
        }
    }

    // Marker strip, painted from last frame's offset; a click scrolls there.
    let total_h = (doc.rows.len().max(1) as f32) * row_h;
    let p = ui.painter_at(strip);
    p.rect_filled(strip, 0.0, th.border.gamma_multiply(0.4));
    let n = doc.rows.len().max(1) as f32;
    for b in &doc.blocks {
        let y0 = strip.top() + b.rows.start as f32 / n * strip.height();
        let y1 = (strip.top() + b.rows.end as f32 / n * strip.height()).max(y0 + 2.0);
        p.rect_filled(egui::Rect::from_x_y_ranges(strip.x_range(), y0..=y1), 0.0, kind_color(b.kind));
    }
    let vy0 = strip.top() + nav.scroll_y / total_h * strip.height();
    let vy1 = vy0 + (view_h / total_h).min(1.0) * strip.height();
    p.rect_stroke(
        egui::Rect::from_x_y_ranges(strip.x_range(), vy0..=vy1),
        0.0,
        egui::Stroke::new(1.0, th.dim),
        egui::StrokeKind::Inside,
    );
    let hit = ui.interact(strip, base.with("diff-strip"), egui::Sense::click_and_drag());
    if let Some(pos) = hit.interact_pointer_pos().filter(|_| hit.clicked() || hit.dragged()) {
        let f = ((pos.y - strip.top()) / strip.height()).clamp(0.0, 1.0);
        target_y = Some(f * total_h - view_h / 2.0);
    }

    // Horizontal bar under the text columns; drags both sides together.
    let track = egui::Rect::from_min_max(egui::pos2(main.left(), main.bottom()), egui::pos2(main.right(), area.bottom()));
    if max_hx > 0.0 {
        let thumb_w = (track.width() * side_w / text_w).max(12.0 * s);
        let span = (track.width() - thumb_w).max(1.0);
        let drag = ui.interact(track, base.with("diff-hbar"), egui::Sense::drag());
        nav.hx += drag.drag_delta().x * max_hx / span;
        nav.hx = nav.hx.clamp(0.0, max_hx);
        let x = track.left() + nav.hx / max_hx * span;
        ui.painter_at(track).rect_filled(
            egui::Rect::from_min_size(egui::pos2(x, track.top() + 1.0), egui::vec2(thumb_w, track.height() - 2.0)),
            2.0 * s,
            th.dim.gamma_multiply(0.6),
        );
    }
    nav.hx = nav.hx.clamp(0.0, max_hx);

    let mut rows_ui = ui.new_child(egui::UiBuilder::new().id_salt("diff-rows").max_rect(main));
    rows_ui.set_clip_rect(main.intersect(ui.clip_rect()));
    rows_ui.spacing_mut().item_spacing.y = 0.0;
    let mut scroll = egui::ScrollArea::vertical()
        .id_salt((base, nav.generation))
        .auto_shrink([false, false]);
    if let Some(y) = target_y {
        scroll = scroll.vertical_scroll_offset(y.max(0.0));
    }
    let hx = nav.hx;
    let out = scroll.show_rows(&mut rows_ui, row_h, doc.rows.len(), |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.0;
        *drawn = range.clone();
        for i in range {
            let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::hover());
            let block = doc.blocks.partition_point(|b| b.rows.end <= i);
            let block = doc.blocks.get(block).filter(|b| b.rows.contains(&i)).map(|b| b.kind);
            paint_row(ui.painter(), r, &doc.rows[i], block, &cols, hx, &font, th);
        }
    });
    nav.scroll_y = out.state.offset.y;
    if jumped {
        nav.jumped_to = Some(nav.scroll_y);
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_row(
    p: &egui::Painter,
    r: egui::Rect,
    row: &Row,
    block: Option<Kind>,
    cols: &Cols,
    hx: f32,
    font: &egui::FontId,
    th: &crate::theme::Theme,
) {
    let tint_for = |side_old: bool| match row.kind {
        Kind::Same => None,
        Kind::Modified => Some(kind_color(Kind::Modified)),
        Kind::Removed if side_old => Some(kind_color(Kind::Removed)),
        Kind::Added if !side_old => Some(kind_color(Kind::Added)),
        _ => None,
    };
    let x = |off: f32| r.left() + off;
    for (cell, num_x, text_x, old_side) in [
        (&row.old, cols.old_num(), cols.old_text(), true),
        (&row.new, cols.new_num(), cols.new_text(), false),
    ] {
        let side = egui::Rect::from_min_max(egui::pos2(x(num_x), r.top()), egui::pos2(x(text_x + cols.side_w), r.bottom()));
        let Some(cell) = cell else {
            if row.kind != Kind::Same {
                p.rect_filled(side, 0.0, th.dim.gamma_multiply(0.06));
            }
            continue;
        };
        let tint = tint_for(old_side);
        if let Some(t) = tint {
            p.rect_filled(side, 0.0, t.gamma_multiply(0.14));
        }
        p.text(
            egui::pos2(x(num_x + cols.num_w - cols.char_w * 0.75), r.center().y),
            egui::Align2::RIGHT_CENTER,
            cell.line.to_string(),
            font.clone(),
            th.dim,
        );
        let text_rect = egui::Rect::from_min_max(egui::pos2(x(text_x), r.top()), side.max);
        let tp = p.with_clip_rect(text_rect.intersect(p.clip_rect()));
        let x0 = text_rect.left() - hx;
        let col_x = |byte: usize| x0 + cell.text[..byte].chars().count() as f32 * cols.char_w;
        if let (Some(t), Some(hot)) = (tint, &cell.hot) {
            tp.rect_filled(egui::Rect::from_x_y_ranges(col_x(hot.start)..=col_x(hot.end), r.y_range()), 0.0, t.gamma_multiply(0.35));
        }
        let skip = (hx / cols.char_w) as usize;
        let take = (cols.side_w / cols.char_w) as usize + 2;
        let vis = visible(&cell.text, skip, take);
        tp.text(
            egui::pos2(x0 + skip as f32 * cols.char_w, r.center().y),
            egui::Align2::LEFT_CENTER,
            &cell.text[vis.clone()],
            font.clone(),
            th.text,
        );
        if cell.no_eol {
            tp.text(
                egui::pos2(col_x(cell.text.len()) + cols.char_w, r.center().y),
                egui::Align2::LEFT_CENTER,
                "(no newline)",
                font.clone(),
                th.dim,
            );
        }
    }
    if let Some(k) = block {
        let g = egui::Rect::from_min_max(
            egui::pos2(x(cols.gutter()), r.top()),
            egui::pos2(x(cols.gutter() + cols.gutter_w), r.bottom()),
        );
        p.rect_filled(g, 0.0, kind_color(k).gamma_multiply(0.30));
    }
}
```

Change the re-export in `src/git_history.rs` to `pub use diff_view::{DiffTarget, DiffView};`.

If an egui call above doesn't compile against 0.34.3 (for example the `rect_stroke` signature or `spacing().scroll` field names), fix it using the **egui-immediate-mode-reference** skill and the egui source under `~/.cargo/registry/src/*/egui-0.34.3/`. Keep the behavior; don't redesign.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --target-dir target/agent git_history::diff_view`
Expected: 11 passed (4 from Task 3 + 7 here). Note the `large_diff_paints_only_viewport_rows` timing line in the output for the docs.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add src/git_history.rs src/git_history/diff_view.rs
git commit -m "feat(history): paint the side-by-side diff view" -m "Cancellable per-target loads, virtualized aligned rows with line numbers,
change bands, word highlight and connector gutter, a marker strip, shared
horizontal scroll, and F7/Shift+F7 difference navigation.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Card: l8th0t"
```

---

### Task 5: Wire the click to a per-Project Diff window

**Files:**
- Modify: `src/git_history/details.rs` (parent on `Details`, click → `DiffTarget`)
- Modify: `src/git_history.rs` (`HistoryAct`, `HistoryView.acts`)
- Modify: `src/wm.rs` (`Content::GitDiff`, opener, drain, snapshot capture/restore, test)
- Modify: `src/workspace.rs` (`ContentSnap::GitDiff`)

**Interfaces:**
- Consumes: `DiffTarget`, `DiffView::{new, retarget, target, show}` (Tasks 3–4)
- Produces:
  - `pub enum HistoryAct { OpenDiff(DiffTarget) }`, `pub acts: Vec<HistoryAct>` on `HistoryView`
  - `WindowManager::open_git_diff_window(&mut self, target: DiffTarget)`, `WindowManager::drain_history_acts(&mut self)`
  - `ContentSnap::GitDiff { commit: String, parent: Option<String>, status: char, old_path: Option<String>, path: String, merge: bool }`

- [ ] **Step 1: Write the failing tests.**

In `details.rs` tests, extend `merge_uses_first_parent_and_empty_detached_commit_has_no_files_or_branches` right after `assert_eq!(details.files[0].path, "topic.txt");`:

```rust
        let target = details.target(0);
        assert_eq!(target.commit, details.hash);
        assert_eq!(target.parent.as_deref(), Some(git(dir, &["rev-parse", "HEAD^1"]).as_str()));
        assert!(target.merge);
        assert_eq!((target.status, target.path.as_str()), ('A', "topic.txt"));
```

and in `real_commit_details_cover_roots_renames_messages_branches_and_worktrees`, after `let renamed = &details.files[2];` assertions:

```rust
        let t = details.target(2);
        assert_eq!((t.status, t.old_path.as_deref(), t.path.as_str()), ('R', Some("src/old name.txt"), "src/new name.txt"));
        assert!(!t.merge);
```

In `src/wm.rs` tests, next to `git_history_resurfaces_and_restores_in_its_own_project`:

```rust
    #[test]
    fn git_diff_window_is_one_per_project_retargeted_and_restored() {
        let ctx = egui::Context::default();
        let tmp = tempfile::tempdir().unwrap();
        let mut m = kanban_desktop(tmp.path().to_path_buf());
        let pid = m.windows[0].id;
        let child = m.project_child_mut(pid).unwrap();
        let target = |path: &str| crate::git_history::DiffTarget {
            commit: "a".repeat(40),
            parent: Some("b".repeat(40)),
            status: 'M',
            old_path: None,
            path: path.into(),
            merge: false,
        };
        // The details click reaches the manager through HistoryView.acts.
        child.open_git_history_window();
        for w in &mut child.windows {
            for t in &mut w.tabs {
                if let Content::GitHistory(v) = &mut t.content {
                    v.acts.push(crate::git_history::HistoryAct::OpenDiff(target("src/one.rs")));
                }
            }
        }
        child.drain_history_acts();
        child.open_git_diff_window(target("src/two.rs"));
        let diffs: Vec<_> = child
            .windows
            .iter()
            .flat_map(|w| &w.tabs)
            .filter(|t| matches!(t.content, Content::GitDiff(_)))
            .collect();
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].title, "Diff: two.rs");
        let Content::GitDiff(v) = &diffs[0].content else { unreachable!() };
        assert_eq!(v.target().unwrap().path, "src/two.rs");

        let json = serde_json::to_string(&m.capture_workspace()).unwrap();
        assert!(json.contains("GitDiff") && json.contains("src/two.rs"));
        let mut back = WindowManager::new().as_desktop();
        back.apply_workspace(&serde_json::from_str(&json).unwrap(), &ctx);
        let Content::Project(child) =
            &back.windows.iter().find(|w| w.is_project()).unwrap().tabs[0].content
        else {
            panic!()
        };
        let restored = child
            .windows
            .iter()
            .flat_map(|w| &w.tabs)
            .find_map(|t| match &t.content {
                Content::GitDiff(v) => v.target().cloned(),
                _ => None,
            })
            .unwrap();
        assert_eq!(restored, target("src/two.rs"));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --target-dir target/agent git_diff_window` and `cargo test --target-dir target/agent git_history::details`
Expected: compile errors (`target`, `acts`, `HistoryAct`, `GitDiff`, `open_git_diff_window`, `drain_history_acts` missing).

- [ ] **Step 3: Details pane.** In `details.rs`:
  - Add fields to `Details`: `parent: Option<String>,` and `opened: Option<usize>,`. In `load`, set `parent: parents.first().map(|p| p.to_string()),` and `opened: None,`.
  - Add:

```rust
impl Details {
    /// The Diff window target for `files[file]`, against the first parent.
    pub(super) fn target(&self, file: usize) -> super::DiffTarget {
        let f = &self.files[file];
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
```

  - Add `open: Option<super::DiffTarget>` to `DetailsView` (it derives `Default`, so no constructor change) and:

```rust
    /// A file row clicked this frame, for `HistoryView` to forward.
    pub(super) fn take_open(&mut self) -> Option<super::DiffTarget> {
        self.open.take()
    }
```

  - In the file-row branch of `show`, make the label clickable and highlight the opened row. Replace the `ui.add(egui::Label::new(...).extend()).on_hover_text(...)` expression with:

```rust
                                let file_index = row.file.unwrap();
                                let mut text = egui::RichText::new(format!(
                                    "{}  {}",
                                    file.status,
                                    display_path(&row.label)
                                ))
                                .color(status_color(file.status));
                                if details.opened == Some(file_index) {
                                    text = text.background_color(th.sel_bg);
                                }
                                let response = ui
                                    .add(egui::Label::new(text).extend().sense(egui::Sense::click()))
                                    .on_hover_text(/* the existing match on &file.previous, unchanged */);
                                if response.clicked() {
                                    clicked = Some(file_index);
                                }
```

  Here `file` is still `&details.files[file_index]`. Declare `let mut clicked = None;` next to `let mut toggled = None;`, and after the `if let Some(index) = toggled { ... }` block add:

```rust
            if let Some(file) = clicked {
                details.opened = Some(file);
                self.open = Some(details.target(file));
            }
```

  If the borrow checker rejects `self.open` inside the `push_id` closure, return `clicked` out of the closure (`ui.push_id(..., |ui| { ...; clicked }).inner`) and do the assignment after it.

- [ ] **Step 4: HistoryView.** In `src/git_history.rs`:

```rust
/// Intents from the history window that change sibling windows; drained by
/// `WindowManager::drain_history_acts` after the draw pass.
pub enum HistoryAct {
    OpenDiff(DiffTarget),
}
```

Add `pub acts: Vec<HistoryAct>,` to `HistoryView` (initialize `acts: Vec::new()` in `new`). At the end of `HistoryView::show`, after `self.details.show(...)`:

```rust
        if let Some(target) = self.details.take_open() {
            self.acts.push(HistoryAct::OpenDiff(target));
        }
```

The Refresh path replaces `self` via `Self::new`, which drops pending acts. That's fine, since a refresh click and a file click can't happen in the same frame.

- [ ] **Step 5: Snapshot variant.** In `src/workspace.rs`, after `GitHistory,` in `ContentSnap`:

```rust
    /// The per-project Diff window and its target; restore re-reads from Git.
    /// A target whose commit is gone restores into the error state.
    GitDiff {
        commit: String,
        parent: Option<String>,
        status: char,
        old_path: Option<String>,
        path: String,
        #[serde(default)]
        merge: bool,
    },
```

- [ ] **Step 6: Window manager.** In `src/wm.rs`:
  - Add the variant after `GitHistory` in `enum Content`:

```rust
    /// Per-project side-by-side diff of one file at one commit (singleton,
    /// retargeted by the Git History details pane).
    GitDiff(crate::git_history::DiffView),
```

  - `Content::show`: `Content::GitDiff(view) => claims_click(ui, |ui| view.show(ui, rect, active, base.with((win_id, "git-diff")))),`
  - Every other exhaustive match that lists `Content::GitHistory(_)`: add `| Content::GitDiff(_)` beside it, in the same arm (`keepalive`, `icon_kind`, the sessions-panel `RowKind::Chat` arm, members, `refresh_exit_titles`, `refresh_auto_titles`, `terminal_groups`). Run `cargo check --target-dir target/agent`; the compiler lists any that were missed.
  - Capture, in `capture_manager`. A Diff window with no target has nothing to restore. Skip it in the **pre-filter** at the top of the tab loop, which runs before the active-index bookkeeping (skipping any later would corrupt `new_active`):

```rust
                if matches!(t.content, Content::TaskManager(_) | Content::Settings(_))
                    || matches!(&t.content, Content::GitDiff(v) if v.target().is_none())
                {
                    continue;
                }
```

  and next to `Content::GitHistory(_) => ContentSnap::GitHistory,`:

```rust
                    Content::GitDiff(view) => {
                        let t = view.target().expect("target-less diff filtered above");
                        ContentSnap::GitDiff {
                            commit: t.commit.clone(),
                            parent: t.parent.clone(),
                            status: t.status,
                            old_path: t.old_path.clone(),
                            path: t.path.clone(),
                            merge: t.merge,
                        }
                    }
```
  - Restore (next to `ContentSnap::GitHistory => ...`):

```rust
                    ContentSnap::GitDiff { commit, parent, status, old_path, path, merge } => {
                        let mut view = crate::git_history::DiffView::new(self.cwd.clone());
                        view.retarget(crate::git_history::DiffTarget {
                            commit: commit.clone(),
                            parent: parent.clone(),
                            status: *status,
                            old_path: old_path.clone(),
                            path: path.clone(),
                            merge: *merge,
                        });
                        Content::GitDiff(view)
                    }
```

  - Opener, after `open_git_history_window`:

```rust
    /// Open or surface the project's singleton Diff window, pointed at `target`.
    fn open_git_diff_window(&mut self, target: crate::git_history::DiffTarget) {
        let title = format!("Diff: {}", target.file_name());
        let found = self.windows.iter().find_map(|w| {
            w.tabs
                .iter()
                .position(|t| matches!(t.content, Content::GitDiff(_)))
                .map(|i| (w.id, i))
        });
        if let Some((win, tab)) = found {
            if let Some(w) = self.windows.iter_mut().find(|w| w.id == win) {
                let t = &mut w.tabs[tab];
                t.title = title;
                if let Content::GitDiff(view) = &mut t.content {
                    view.retarget(target);
                }
            }
            self.surface_target(crate::panel::TargetPath {
                project: win,
                ptab: None,
                window: None,
                tab: Some(tab),
            });
            self.mark_workspace_dirty();
            return;
        }
        let (id, rect) = self.next_slot(egui::vec2(1100.0, 640.0));
        let mut view = crate::git_history::DiffView::new(self.cwd.clone());
        view.retarget(target);
        self.push_win(id, Tab::fixed(title, Content::GitDiff(view)), rect);
        self.mark_workspace_dirty();
    }

    /// Apply Git History intents recorded during the draw: the details pane
    /// and the Diff window are siblings inside one project.
    fn drain_history_acts(&mut self) {
        let mut acts = Vec::new();
        for w in &mut self.windows {
            for t in &mut w.tabs {
                if let Content::GitHistory(v) = &mut t.content {
                    acts.append(&mut v.acts);
                }
            }
        }
        for act in acts {
            match act {
                crate::git_history::HistoryAct::OpenDiff(target) => self.open_git_diff_window(target),
            }
        }
    }
```

  - Call it in `show` right after `self.drain_plan_acts();`: `self.drain_history_acts();`

- [ ] **Step 7: Run to verify pass**

Run: `cargo test --target-dir target/agent git_diff_window`, then `cargo test --target-dir target/agent git_history`, then the full `cargo test --target-dir target/agent`.
Expected: all pass. The full suite is the gate for the new `Content` arms.

- [ ] **Step 8: Commit**

```bash
cargo fmt
git add src/git_history.rs src/git_history/details.rs src/wm.rs src/workspace.rs
git commit -m "feat(history): open a changed file in the per-project Diff window" -m "Clicking a file in commit details retargets one reusable Diff window,
which tiles, tabs, zooms and restores with its target.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Card: l8th0t"
```

---

### Task 6: Docs, glossary, and native evidence

**Files:**
- Modify: `docs/git-history.md`
- Modify: `CONTEXT.md`

- [ ] **Step 1: Update `docs/git-history.md`.** Replace the paragraph starting "Branch selection and checkout/switching belong to the next task." with:

```markdown
Click a file in the changed-file tree to open it in the project's Diff window.
Branch selection, checkout, search, context menus, and other Git operations are
outside this viewer's scope.

## Diff window

One Diff window per Project, reused: clicking another file retargets it. It
tiles, tabs, zooms and restores (with its file) like any viewer. It shows the
whole file side by side, old left and new right, with both line numbers.
Changed regions are banded: red removed, green added, amber modified, with the
changed middle of a modified line tinted stronger. A gutter band links each
change across the panes; a strip on the right marks every change in the file
and outlines the viewport (click it to jump). The header shows the path
(`old → new` for renames), the commits compared, and "N differences".

Keys while the window is focused: F7 / Shift+F7 next/previous difference;
Up/Down/PgUp/PgDn/Home/End scroll. Shift+wheel scrolls both sides
horizontally together; so does the thin bar under the text.

Binary files, submodules, diffs over 16 MiB or that git cannot return as one
whole-file hunk (a change more than 200,000 lines from the file start or from
the next change), and
changes with no content difference (pure renames, mode changes, empty adds)
show a one-line notice instead.

Gotchas:
- Git computes the diff (`diff-tree -p` for A/D/M, the `<rev>:<path>` blob
  form for T/R/C) with `-U200000`. Never raise `-U` toward `i32::MAX`: git
  2.39 emits overlapping repeated hunks there. The parser accepts exactly one
  whole-file hunk and turns anything else into the too-large notice.
- CRLF is shown as LF, so a pure line-ending change shows Modified rows with
  nothing highlighted inside them. Tabs are expanded to 4 columns.
- The monospace grid assumes one column per char; wide CJK glyphs render wider
  than their column and can misalign highlights on that line.
- No syntax highlighting, text selection, or copy (v1).
```

Add to "Key files":

```markdown
- `src/git_history/git.rs`: the shared Git subprocess helper (spawn, capped
  drains, cancel/timeout watchdog) used by the history stream, details, and diff.
- `src/git_history/diff.rs`: pure unified-diff parser into aligned rows and blocks.
- `src/git_history/diff_view.rs`: `DiffTarget`, the diff reads, and `DiffView`.
```

and extend the `src/wm.rs` and `src/workspace.rs` bullets with `Content::GitDiff`, `open_git_diff_window`, `drain_history_acts`, and `ContentSnap::GitDiff` (target persisted; restore re-reads). Add the Task 4 timing line to "Validation", dated, in the same style as the 100,000-commit paragraph. Update the test command's description to include `diff` and `diff_view`.

- [ ] **Step 2: Glossary.** Append to `CONTEXT.md` after **Commit details**:

```markdown

**Diff window**:
The per-Project, reused side-by-side view of one changed file at one commit,
opened from Commit details. Owned by `src/git_history/diff_view.rs`; rows are
padded so both sides align, and git's single whole-file hunk is the only
accepted input.
```

- [ ] **Step 3: Native evidence.** Ask the human to run **build-screenshot** (user-only). Target states: a modified file with several differences after pressing F7, a rename, and a notice (for example a binary file). Look at each screenshot before claiming the feature works.

- [ ] **Step 4: Commit**

```bash
git add docs/git-history.md CONTEXT.md
git commit -m "docs(history): document the Diff window" -m "Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Card: l8th0t"
```
