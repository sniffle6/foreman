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
    let stderr =
        std::thread::spawn(move || drain(pipe, STDERR_CAP).map(|(b, _)| b).unwrap_or_default());
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
            output(
                repo.path(),
                args,
                &flag(false),
                cap,
                Duration::from_secs(30),
            )
        };
        assert_eq!(
            read(&["cat-file", "-p", &blob], 1024),
            Err(GitError::TooLarge)
        );
        assert_eq!(
            read(&["cat-file", "-p", &blob], 8 << 20).unwrap().len(),
            4 << 20
        );
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
