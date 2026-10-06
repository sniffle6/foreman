//! Git subprocesses. Reads (`spawn`, `output`): capped pipe drains and a
//! watchdog that kills and reaps the child on cancel or timeout. Writes
//! (`write`): the Changes window's stage, commit and push; a commit runs to
//! completion, a push can be cancelled or timed out, taking its whole
//! process tree (a credential helper's window included) with it.
use std::io::{Read, Write};
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
/// Each of a write's stdout and stderr: room for a chatty hook.
const WRITE_OUTPUT_CAP: usize = 64 * 1024;

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
            Self::Cancelled => f.write_str("Git cancelled"),
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

/// `git --no-pager <args>` in `cwd`, piped, with no console window and no
/// credential prompt on the (absent) terminal.
fn command(cwd: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(cwd)
        .arg("--no-pager")
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd
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
    let mut cmd = command(cwd, args);
    // Reads never take the index lock or refresh the index, so they never
    // wake the repository watch.
    cmd.env("GIT_OPTIONAL_LOCKS", "0").stdin(Stdio::null());
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

/// Worker-only write: run `git <args>`, feeding `stdin` if any. Returns
/// stdout then stderr, trimmed; a non-zero exit is `Failed` with the same
/// text, since hooks and "nothing to commit" print to stdout.
///
/// With a `cancel` flag or a `timeout` the child goes into a kill-on-close
/// job object right after spawn, and cancel or timeout drops it: that kills
/// `git.exe` and everything it spawned (an `ssh`, a credential helper and
/// its sign-in window) in one stroke, where `Child::kill` alone would leave
/// the grandchildren running. Only a push passes either, since killing a
/// push leaves nothing locked locally. A commit or `add` passes neither and
/// simply waits: killed mid-write it would leave `index.lock` behind and
/// wedge the repository.
pub(super) fn write(
    cwd: &Path,
    args: &[&str],
    stdin: Option<&str>,
    cancel: Option<&Arc<AtomicBool>>,
    timeout: Option<Duration>,
) -> Result<String, GitError> {
    if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
        return Err(GitError::Cancelled);
    }
    let mut cmd = command(cwd, args);
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let mut child = cmd.spawn().map_err(|e| GitError::Spawn(e.to_string()))?;
    let killable = cancel.is_some() || timeout.is_some();
    // Best-effort (`None` when Windows refuses): then only git.exe itself
    // dies on kill, the pre-job behavior.
    let job = killable
        .then(|| crate::job::Job::assign(child.id()))
        .flatten();
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let text = text.to_owned();
        // Its own thread: a large message must not deadlock against the
        // output pipes. Dropping the pipe closes stdin.
        std::thread::spawn(move || pipe.write_all(text.as_bytes()));
    }
    let out = child.stdout.take().unwrap();
    let err = child.stderr.take().unwrap();
    let stdout = std::thread::spawn(move || {
        drain(out, WRITE_OUTPUT_CAP)
            .map(|(b, _)| b)
            .unwrap_or_default()
    });
    let stderr = std::thread::spawn(move || {
        drain(err, WRITE_OUTPUT_CAP)
            .map(|(b, _)| b)
            .unwrap_or_default()
    });
    let status = if killable {
        let start = Instant::now();
        let verdict = loop {
            if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
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
        match verdict {
            Ok(status) => status,
            Err(e) => {
                // Kill-on-close takes the tree; `kill` covers a missing job.
                drop(job);
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        }
    } else {
        child.wait().map_err(|e| GitError::Io(e.to_string()))?
    };
    let mut text = String::from_utf8_lossy(&stdout.join().unwrap_or_default()).into_owned();
    let stderr = stderr.join().unwrap_or_default();
    if !stderr.is_empty() {
        if !text.trim().is_empty() {
            text.push('\n');
        }
        text.push_str(&String::from_utf8_lossy(&stderr));
    }
    let text = text.trim().to_owned();
    if status.success() {
        Ok(text)
    } else {
        Err(GitError::Failed(text))
    }
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
