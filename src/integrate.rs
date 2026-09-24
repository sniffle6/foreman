//! Repository integration queue (spec: worktree-integration-queue). Workers
//! submit a card's committed worktree; Foreman gives each repository one
//! integration turn at a time — rebase the worktree onto the card's base,
//! run the project's configured checks, fast-forward the destination
//! checkout — and hands conflicts back to the worker without blocking the
//! cards behind it.
//!
//! GUI-free: everything here is driven by `wm.rs` (the per-project
//! coordinator thread, card bookkeeping, toasts) and `control.rs` (the
//! `integrate` verb). Git runs through [`crate::kanban::git`] for short
//! probes and [`run_child`] for anything long-lived, so a Foreman crash
//! takes its children with it (kill-on-close Job) rather than leaving a
//! rebase or a test run to overlap the next owner.
//!
//! Durable state is one JSON file per card under the repository's git
//! common directory (`<common>/foreman/integration/`), shared by every
//! linked worktree and every app instance on the clone and never tracked.
//! Two OS file locks live beside them: `meta.lock` guards queue writes for
//! milliseconds; `turn.lock` is held by the integrating owner for the whole
//! turn and released by the OS when that process dies — process-lifetime
//! ownership, not an expiring lease that could grant a second turn while
//! the first still runs.

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Request file schema version. Bump only alongside a documented migration.
pub const REQUEST_V: u32 = 1;

/// How often a project's coordinator re-reads the queue when nothing has
/// kicked it (a submission or a finished turn kicks it immediately).
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// After the destination refused a turn (dirty, wrong branch, busy) the
/// coordinator waits this long before trying again; a new submission
/// clears the wait.
pub const HOLD_BACKOFF: Duration = Duration::from_secs(15);

/// A target that moves under a prepared commit is re-prepared and
/// re-validated this many times within one turn before the request goes
/// back to the queue with a note.
pub const MAX_TARGET_RETRIES: u32 = 3;

/// Check-command timeout when the policy file names none.
pub const DEFAULT_CHECK_TIMEOUT_SECS: u64 = 1800;

/// Per-repository check policy, read from the destination checkout.
pub const POLICY_FILE: &str = ".foreman/integrate.json";

/// How long a queue write waits for `meta.lock` before giving up.
const META_LOCK_WAIT: Duration = Duration::from_secs(3);

/// A single git operation (rebase, fast-forward) that runs longer than this
/// is killed and reported as a process failure.
const GIT_OP_TIMEOUT: Duration = Duration::from_secs(300);

/// Lines of check output kept on the request for the worker to read.
const OUTPUT_TAIL_LINES: usize = 40;
const FAILURE_LINES_MAX: usize = 12;
const FAILURE_LINE_CHARS: usize = 240;

fn is_false(b: &bool) -> bool {
    !*b
}

fn short(sha: &str) -> &str {
    sha.get(..8).unwrap_or(sha)
}

/// Where a request is in its life. Persisted; the board and `kanban list`
/// show it as a substate of In Progress — no new column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Submitted, waiting for the repository's turn.
    Queued,
    /// An owner holds the turn and is working in the card's worktree.
    Integrating,
    /// Handed back: the worker must act, then resubmit.
    NeedsResolution,
    /// The prepared commit is on the target; card bookkeeping is pending.
    Integrated,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::Queued => "queued",
            Phase::Integrating => "integrating",
            Phase::NeedsResolution => "needs resolution",
            Phase::Integrated => "integrated",
        }
    }

    /// The queue owns the worktree: nothing else may run git in it
    /// (teardown, restart, rm, discard all wait or refuse).
    pub fn owns_worktree(self) -> bool {
        matches!(self, Phase::Queued | Phase::Integrating)
    }
}

/// Why a request left the happy path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// The rebase stopped on conflicts; the worktree is left mid-rebase.
    Conflict,
    /// The configured check command failed (or timed out).
    CheckFailed,
    /// The worktree is not at the submitted commit, not on its branch, or
    /// has uncommitted changes.
    SourceChanged,
    /// A git operation is in progress in the worktree.
    SourceBusy,
    /// The worktree directory is gone.
    SourceMissing,
    /// The destination checkout has uncommitted changes (held, retried).
    DestinationDirty,
    /// The destination checkout is not on the target branch (held, retried).
    DestinationBranch,
    /// A git operation is in progress in the destination (held, retried).
    DestinationBusy,
    /// The target moved more than [`MAX_TARGET_RETRIES`] times in one turn.
    TargetMoved,
    /// git or the check could not run at all; the detail carries its output.
    ProcessFailed,
    /// A previous owner died mid-rebase; the worktree needs a human/worker.
    Interrupted,
    /// Branch mode: the target is no longer an ancestor of the card's
    /// branch. The queue never rebases the shared checkout; the worker does.
    BaseMoved,
}

impl Reason {
    pub fn label(self) -> &'static str {
        match self {
            Reason::Conflict => "conflict",
            Reason::CheckFailed => "check failed",
            Reason::SourceChanged => "source changed",
            Reason::SourceBusy => "source busy",
            Reason::SourceMissing => "source missing",
            Reason::DestinationDirty => "destination dirty",
            Reason::DestinationBranch => "destination branch",
            Reason::DestinationBusy => "destination busy",
            Reason::TargetMoved => "target moved",
            Reason::ProcessFailed => "process failed",
            Reason::Interrupted => "interrupted",
            Reason::BaseMoved => "base moved",
        }
    }

    /// What the worker (or human) does about it.
    pub fn next_action(self) -> &'static str {
        match self {
            Reason::Conflict => {
                "resolve the conflicts in the worktree, `git add` them, `git rebase --continue`, then resubmit"
            }
            Reason::CheckFailed => "fix the failing check in the worktree, commit, then resubmit",
            Reason::SourceChanged => {
                "commit or discard the changes in the worktree, then resubmit the branch as it is now"
            }
            Reason::SourceBusy => {
                "finish or abort the git operation in the worktree, then resubmit"
            }
            Reason::SourceMissing => "restore the worktree (restart the card), then resubmit",
            Reason::DestinationDirty => {
                "commit or stash the destination checkout's changes; the queue retries on its own"
            }
            Reason::DestinationBranch => {
                "check the target branch out in the destination; the queue retries on its own"
            }
            Reason::DestinationBusy => {
                "finish the git operation in the destination; the queue retries on its own"
            }
            Reason::TargetMoved => "nothing: the queue retries on its next turn",
            Reason::ProcessFailed => "read the detail, fix the cause, then resubmit",
            Reason::Interrupted => {
                "in the worktree run `git rebase --abort` (or finish the rebase), then resubmit"
            }
            Reason::BaseMoved => {
                "in the checkout run `git rebase <base>` (block if uncommitted changes that are not yours stop it), commit, then resubmit"
            }
        }
    }
}

/// The recorded end of an attempt that did not integrate.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Outcome {
    pub reason: Reason,
    pub detail: String,
    pub next: String,
    pub at: String,
}

/// Where an attempt is inside its turn. Persisted before each stage so
/// recovery knows what may already have happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Preflight,
    Rebase,
    Check,
    Merge,
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Stage::Preflight => "preflight",
            Stage::Rebase => "rebase",
            Stage::Check => "check",
            Stage::Merge => "merge",
        }
    }
}

/// One check run, tied to the exact prepared commit and target it ran
/// against — never inferred from an older worker test report.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CheckRecord {
    /// Empty when no check is configured.
    pub command: Vec<String>,
    pub ok: bool,
    pub tail: String,
    pub prepared: String,
    pub target: String,
}

/// One owner's attempt at a request. `pid`/`run` name the owner (a Foreman
/// process, by its app-run nonce) so a recovered file says who died.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Attempt {
    pub id: String,
    pub pid: u32,
    pub run: String,
    pub started: String,
    pub stage: Stage,
    #[serde(default)]
    pub retries: u32,
    /// The target commit the rebase was prepared against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_commit: Option<String>,
    /// HEAD of the worktree after a clean rebase — what the check ran on
    /// and what the fast-forward advances to. Persisted before the merge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<CheckRecord>,
}

/// One durable submission: a file `<common>/foreman/integration/<card>.json`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Request {
    pub v: u32,
    /// Durable submission order across the repository; assigned under the
    /// meta lock, never reused.
    pub seq: u64,
    pub card: String,
    /// The canonical git common directory — the repository's identity.
    pub repo: String,
    /// The branch to advance (the card's recorded base).
    pub target: String,
    /// The destination checkout's root (where `target` is checked out).
    pub dest: String,
    /// The card's worktree path.
    pub worktree: String,
    /// The card's branch (`card/<id>`).
    pub branch: String,
    /// The worktree's HEAD at submission; a moved source is refused.
    pub commit: String,
    /// The submitting terminal, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<String>,
    pub submitted: String,
    pub updated: String,
    pub phase: Phase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<Attempt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    /// Still queued, but the destination refused the last turn: why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold: Option<String>,
    /// A cancel arrived while an owner held the turn; honored at the next
    /// checkpoint.
    #[serde(default, skip_serializing_if = "is_false")]
    pub cancel_requested: bool,
    /// What recovery did to this request, for the human.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Branch mode (spec: dispatch-branch): `worktree` is the destination
    /// checkout itself, on `branch`. Integrated by [`integrate_in_place`]:
    /// no rebase, a ref-only fast-forward, and the checkout's HEAD moved
    /// back to `target` without touching a file.
    #[serde(default, skip_serializing_if = "is_false")]
    pub in_place: bool,
}

impl Request {
    fn touch(&mut self) {
        self.updated = crate::kanban::now_stamp();
    }
}

/// The request-side facts a submission needs, gathered by
/// [`prepare_submission`] with the source preflight already passed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submission {
    pub card: String,
    pub repo: String,
    pub target: String,
    pub dest: String,
    pub worktree: String,
    pub branch: String,
    pub commit: String,
    pub worker: Option<String>,
    pub in_place: bool,
}

/// What [`Queue::submit`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submitted {
    pub request: Request,
    /// True when an equal request (same card, same commit) already stood;
    /// nothing was written.
    pub existing: bool,
}

/// What [`Queue::cancel`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cancelled {
    /// The request was queued or handed back: removed outright.
    Removed,
    /// An owner is mid-turn: flagged; it stops at its next checkpoint.
    Requested,
}

/// The owner identity stamped on an attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owner {
    pub pid: u32,
    pub run: String,
}

impl Owner {
    pub fn this_process() -> Self {
        Owner {
            pid: std::process::id(),
            run: crate::kanban::run_nonce().to_string(),
        }
    }
}

/// The card-facing projection of a request: what `kanban list --json`
/// prints as `integration` and what the board paints. Derived, never
/// stored on the card.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IntegrationView {
    pub phase: Phase,
    pub commit: String,
    /// 1-based place among queued requests; absent unless queued.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<Stage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<Reason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared: Option<String>,
    /// `none configured`, `passed`, or `failed` once a check has run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub cancel_requested: bool,
}

impl IntegrationView {
    fn from_request(r: &Request, position: Option<usize>) -> Self {
        let attempt = r.attempt.as_ref();
        IntegrationView {
            phase: r.phase,
            commit: r.commit.clone(),
            position,
            stage: attempt.map(|a| a.stage),
            reason: r.outcome.as_ref().map(|o| o.reason),
            detail: r.outcome.as_ref().map(|o| o.detail.clone()),
            next: r.outcome.as_ref().map(|o| o.next.clone()),
            hold: r.hold.clone(),
            prepared: attempt.and_then(|a| a.prepared.clone()),
            checks: attempt.and_then(|a| a.check.as_ref()).map(|c| {
                if c.command.is_empty() {
                    "none configured".to_string()
                } else if c.ok {
                    "passed".to_string()
                } else {
                    "failed".to_string()
                }
            }),
            note: r.note.clone(),
            cancel_requested: r.cancel_requested,
        }
    }

    /// One short line: `queued #2`, `integrating · check`,
    /// `needs resolution · conflict: conflicts in: f.txt`, `integrated`.
    pub fn summary(&self) -> String {
        let mut s = match self.phase {
            Phase::Queued => {
                let mut s = match self.position {
                    Some(p) => format!("queued #{p}"),
                    None => "queued".to_string(),
                };
                if let Some(h) = &self.hold {
                    s.push_str(" · held: ");
                    s.push_str(&first_line(h));
                }
                s
            }
            Phase::Integrating => match self.stage {
                Some(st) => format!("integrating · {}", st.label()),
                None => "integrating".to_string(),
            },
            Phase::NeedsResolution => {
                let mut s = "needs resolution".to_string();
                if let Some(r) = self.reason {
                    s.push_str(" · ");
                    s.push_str(r.label());
                }
                if let Some(d) = &self.detail {
                    s.push_str(": ");
                    s.push_str(&first_line(d));
                }
                s
            }
            Phase::Integrated => "integrated".to_string(),
        };
        if self.cancel_requested {
            s.push_str(" · cancelling");
        }
        s
    }

    /// The `[integrate …]` tail `kanban list` appends to a card's line.
    pub fn tail(&self) -> String {
        format!("[integrate {}]", self.summary())
    }
}

fn first_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    if line.chars().count() > 96 {
        let cut: String = line.chars().take(93).collect();
        format!("{cut}…")
    } else {
        line.to_string()
    }
}

/// Views for every request, keyed by card: queued positions count in `seq`
/// order. Pure.
pub fn views(requests: &[Request]) -> HashMap<String, IntegrationView> {
    let mut sorted: Vec<&Request> = requests.iter().collect();
    sorted.sort_by_key(|r| r.seq);
    let mut pos = 0;
    let mut out = HashMap::new();
    for r in sorted {
        let position = (r.phase == Phase::Queued).then(|| {
            pos += 1;
            pos
        });
        out.insert(r.card.clone(), IntegrationView::from_request(r, position));
    }
    out
}

// ---------------------------------------------------------------------------
// Durable queue: one file per card, two locks, atomic writes.
// ---------------------------------------------------------------------------

/// The repository's queue directory. Cheap to construct; every method
/// touches the filesystem afresh, because other processes write here too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Queue {
    dir: PathBuf,
}

/// The repository's integration turn. Dropping it (or the owning process
/// dying) releases the OS lock.
pub struct Turn {
    _lock: File,
}

/// The short queue-write lock.
struct MetaLock {
    _lock: File,
}

impl Queue {
    /// The queue for a repository named by its git common directory.
    pub fn for_repo(common_dir: &Path) -> Self {
        Queue {
            dir: common_dir.join("foreman").join("integration"),
        }
    }

    /// A queue at an explicit directory (tests, tools).
    pub fn at(dir: PathBuf) -> Self {
        Queue { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn ensure_dir(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| format!("cannot create {}: {e}", self.dir.display()))
    }

    fn open_lock(&self, name: &str) -> Result<File, String> {
        self.ensure_dir()?;
        let path = self.dir.join(name);
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| format!("cannot open {}: {e}", path.display()))
    }

    /// Take the queue-write lock, waiting briefly for another writer.
    fn meta(&self) -> Result<MetaLock, String> {
        let f = self.open_lock("meta.lock")?;
        let deadline = Instant::now() + META_LOCK_WAIT;
        loop {
            match f.try_lock() {
                Ok(()) => return Ok(MetaLock { _lock: f }),
                Err(std::fs::TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        return Err("integration queue is busy; retry".into());
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(std::fs::TryLockError::Error(e)) => {
                    return Err(format!("cannot lock the integration queue: {e}"));
                }
            }
        }
    }

    /// Try to take the repository's integration turn without waiting.
    /// `Ok(None)` = another owner (thread or process) holds it.
    pub fn try_turn(&self) -> Result<Option<Turn>, String> {
        let f = self.open_lock("turn.lock")?;
        match f.try_lock() {
            Ok(()) => Ok(Some(Turn { _lock: f })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => {
                Err(format!("cannot lock the integration turn: {e}"))
            }
        }
    }

    fn request_path(&self, card: &str) -> PathBuf {
        self.dir.join(format!("{card}.json"))
    }

    /// Every request on disk, oldest submission first. Reads need no lock:
    /// writes are atomic renames, so a reader sees whole files only.
    pub fn requests(&self) -> Vec<Request> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut out: Vec<Request> = entries
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
            .filter_map(|e| {
                let text = std::fs::read_to_string(e.path()).ok()?;
                match serde_json::from_str::<Request>(&text) {
                    Ok(r) => Some(r),
                    Err(err) => {
                        eprintln!(
                            "integrate: skipping unparseable request {}: {err}",
                            e.path().display()
                        );
                        None
                    }
                }
            })
            .collect();
        out.sort_by_key(|r| r.seq);
        out
    }

    pub fn get(&self, card: &str) -> Option<Request> {
        let text = std::fs::read_to_string(self.request_path(card)).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Atomic write: temp file in the same dir, then rename.
    fn write(&self, req: &Request) -> Result<(), String> {
        self.ensure_dir()?;
        let path = self.request_path(&req.card);
        let tmp = self.dir.join(format!("{}.json.tmp", req.card));
        let json = serde_json::to_string_pretty(req).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, json).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path)
            .map_err(|e| format!("cannot finalize {}: {e}", path.display()))?;
        Ok(())
    }

    /// Persist progress made by the turn owner, keeping a cancel flag that
    /// arrived on disk meanwhile.
    fn save_progress(&self, req: &mut Request) -> Result<(), String> {
        let _m = self.meta()?;
        if let Some(cur) = self.get(&req.card) {
            req.cancel_requested |= cur.cancel_requested;
        }
        req.touch();
        self.write(req)
    }

    /// The owner's cancel checkpoint: a flagged or vanished file cancels.
    fn cancel_pending(&self, card: &str) -> bool {
        self.get(card).is_none_or(|r| r.cancel_requested)
    }

    /// Next durable sequence number: the counter file, or the highest seq
    /// on disk, plus one — so a removed request never frees its number.
    fn next_seq(&self) -> Result<u64, String> {
        let path = self.dir.join("seq");
        let counter: u64 = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        let on_disk = self.requests().iter().map(|r| r.seq).max().unwrap_or(0);
        let next = counter.max(on_disk) + 1;
        let tmp = self.dir.join("seq.tmp");
        std::fs::write(&tmp, next.to_string())
            .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path)
            .map_err(|e| format!("cannot finalize {}: {e}", path.display()))?;
        Ok(next)
    }

    /// Queue a submission at the tail. Idempotent for the same card and
    /// commit while it is queued, integrating, or already integrated; a
    /// handed-back request (or a queued one for an older commit) is
    /// replaced by a fresh tail entry — explicit resubmission joins the
    /// tail, never the head. An integrating request for another commit is
    /// refused: cancel it first.
    pub fn submit(&self, sub: Submission) -> Result<Submitted, String> {
        let _m = self.meta()?;
        if let Some(existing) = self.get(&sub.card) {
            let same = existing.commit == sub.commit;
            match existing.phase {
                Phase::Integrating => {
                    if same {
                        return Ok(Submitted {
                            request: existing,
                            existing: true,
                        });
                    }
                    return Err(format!(
                        "card {} is integrating commit {}; cancel it first (integrate {} --cancel)",
                        sub.card,
                        short(&existing.commit),
                        sub.card
                    ));
                }
                Phase::Integrated => {
                    return Ok(Submitted {
                        request: existing,
                        existing: true,
                    });
                }
                Phase::Queued if same => {
                    return Ok(Submitted {
                        request: existing,
                        existing: true,
                    });
                }
                Phase::Queued | Phase::NeedsResolution => {}
            }
        }
        let seq = self.next_seq()?;
        let now = crate::kanban::now_stamp();
        let request = Request {
            v: REQUEST_V,
            seq,
            card: sub.card,
            repo: sub.repo,
            target: sub.target,
            dest: sub.dest,
            worktree: sub.worktree,
            branch: sub.branch,
            commit: sub.commit,
            worker: sub.worker,
            submitted: now.clone(),
            updated: now,
            phase: Phase::Queued,
            attempt: None,
            outcome: None,
            hold: None,
            cancel_requested: false,
            note: None,
            in_place: sub.in_place,
        };
        self.write(&request)?;
        Ok(Submitted {
            request,
            existing: false,
        })
    }

    /// Cancel a card's request. Queued or handed-back work is removed at
    /// once; an integrating request is flagged and stops at its owner's
    /// next checkpoint; an integrated one cannot be cancelled (the target
    /// already moved).
    pub fn cancel(&self, card: &str) -> Result<Cancelled, String> {
        let _m = self.meta()?;
        let Some(mut req) = self.get(card) else {
            return Err(format!("card {card} has no integration request"));
        };
        match req.phase {
            Phase::Integrated => Err(format!(
                "card {card} is already integrated; bookkeeping will finish on its own"
            )),
            Phase::Integrating => {
                req.cancel_requested = true;
                req.touch();
                self.write(&req)?;
                Ok(Cancelled::Requested)
            }
            Phase::Queued | Phase::NeedsResolution => {
                self.remove_file(card)?;
                Ok(Cancelled::Removed)
            }
        }
    }

    /// Drop a request whose bookkeeping is complete (or that a cancel
    /// removed). Missing is fine — another instance may have done it.
    pub fn remove(&self, card: &str) -> Result<(), String> {
        let _m = self.meta()?;
        self.remove_file(card)
    }

    fn remove_file(&self, card: &str) -> Result<(), String> {
        match std::fs::remove_file(self.request_path(card)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("cannot remove request for {card}: {e}")),
        }
    }

    /// The card-facing views of every request.
    pub fn views(&self) -> HashMap<String, IntegrationView> {
        views(&self.requests())
    }
}

// ---------------------------------------------------------------------------
// Submission: repository identity + source preflight.
// ---------------------------------------------------------------------------

/// The repository's identity: its canonical git common directory, the one
/// thing the main checkout and every linked worktree share. Absolute,
/// canonical, without the `\\?\` verbatim prefix.
pub fn repo_identity(cwd: &Path) -> Result<PathBuf, String> {
    let common = crate::kanban::git(cwd, &["rev-parse", "--git-common-dir"])?;
    let p = PathBuf::from(&common);
    let p = if p.is_absolute() { p } else { cwd.join(p) };
    let canon = std::fs::canonicalize(&p)
        .map_err(|e| format!("cannot resolve repository {}: {e}", p.display()))?;
    Ok(strip_verbatim(canon))
}

fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => p,
    }
}

/// Which git operation, if any, is in progress in `tree` (the per-worktree
/// git dir holds the markers). `None` when git cannot answer — a missing
/// tree is caught earlier by the callers.
pub(crate) fn git_op_in_progress(tree: &Path) -> Option<&'static str> {
    let gitdir = crate::kanban::git(tree, &["rev-parse", "--absolute-git-dir"]).ok()?;
    let g = Path::new(&gitdir);
    [
        ("rebase-merge", "rebase"),
        ("rebase-apply", "rebase"),
        ("MERGE_HEAD", "merge"),
        ("CHERRY_PICK_HEAD", "cherry-pick"),
        ("REVERT_HEAD", "revert"),
        ("BISECT_LOG", "bisect"),
    ]
    .into_iter()
    .find(|(marker, _)| g.join(marker).exists())
    .map(|(_, label)| label)
}

/// The source worktree is present, idle, on `branch`, clean, and — when
/// `expect` is given — still at that commit. Returns HEAD.
fn source_preflight(
    tree: &Path,
    branch: &str,
    expect: Option<&str>,
) -> Result<String, (Reason, String)> {
    if !tree.join(".git").exists() {
        return Err((
            Reason::SourceMissing,
            format!("worktree {} is missing", tree.display()),
        ));
    }
    if let Some(op) = git_op_in_progress(tree) {
        return Err((
            Reason::SourceBusy,
            format!("a git {op} is in progress in the worktree"),
        ));
    }
    let on = crate::kanban::git(tree, &["symbolic-ref", "--short", "HEAD"]).map_err(|e| {
        (
            Reason::SourceChanged,
            format!("worktree is not on a branch: {e}"),
        )
    })?;
    if on != branch {
        return Err((
            Reason::SourceChanged,
            format!("worktree is on {on}, expected {branch}"),
        ));
    }
    let head =
        crate::kanban::git(tree, &["rev-parse", "HEAD"]).map_err(|e| (Reason::ProcessFailed, e))?;
    if let Some(x) = expect
        && x != head
    {
        return Err((
            Reason::SourceChanged,
            format!(
                "worktree moved from {} to {} after submission",
                short(x),
                short(&head)
            ),
        ));
    }
    let dirty = crate::kanban::git(tree, &["status", "--porcelain", "--untracked-files=no"])
        .map_err(|e| (Reason::ProcessFailed, e))?;
    if !dirty.trim().is_empty() {
        return Err((
            Reason::SourceChanged,
            format!("uncommitted changes in the worktree: {}", name_list(&dirty)),
        ));
    }
    Ok(head)
}

/// Branch mode's source preflight: the project checkout is present, idle,
/// on `branch`, and — when `expect` is given — still at that commit.
/// Uncommitted changes are allowed: they may be the human's, and nothing in
/// the in-place turn touches the working tree. Returns HEAD.
fn source_preflight_in_place(
    tree: &Path,
    branch: &str,
    expect: Option<&str>,
) -> Result<String, (Reason, String)> {
    if !tree.is_dir() {
        return Err((
            Reason::SourceMissing,
            format!("checkout {} is missing", tree.display()),
        ));
    }
    if let Some(op) = git_op_in_progress(tree) {
        return Err((
            Reason::SourceBusy,
            format!("a git {op} is in progress in the checkout"),
        ));
    }
    let on = crate::kanban::git(tree, &["symbolic-ref", "--short", "HEAD"]).map_err(|e| {
        (
            Reason::SourceChanged,
            format!("checkout is not on a branch: {e}"),
        )
    })?;
    if on != branch {
        return Err((
            Reason::SourceChanged,
            format!("checkout is on {on}, expected {branch}"),
        ));
    }
    let head =
        crate::kanban::git(tree, &["rev-parse", "HEAD"]).map_err(|e| (Reason::ProcessFailed, e))?;
    if let Some(x) = expect
        && x != head
    {
        return Err((
            Reason::SourceChanged,
            format!(
                "{branch} moved from {} to {} after submission",
                short(x),
                short(&head)
            ),
        ));
    }
    Ok(head)
}

/// Branch mode's target preflight: `target` exists and is checked out in no
/// worktree (moving a ref another tree has checked out would leave that
/// tree's files behind its HEAD — held, retried). Returns its commit.
fn target_preflight(tree: &Path, target: &str) -> Result<String, (Reason, String)> {
    let target_ref = format!("refs/heads/{target}");
    let listed = crate::kanban::git(tree, &["worktree", "list", "--porcelain"])
        .map_err(|e| (Reason::ProcessFailed, e))?;
    let mut path = None;
    for line in listed.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(p.trim().to_string());
        } else if line.strip_prefix("branch ").map(str::trim) == Some(target_ref.as_str()) {
            return Err((
                Reason::DestinationBranch,
                format!(
                    "{target} is checked out in {}; switch that tree off it",
                    path.unwrap_or_default()
                ),
            ));
        }
    }
    crate::kanban::git(tree, &["rev-parse", "--verify", "--quiet", &target_ref]).map_err(|_| {
        (
            Reason::ProcessFailed,
            format!("target branch {target} does not exist"),
        )
    })
}

/// The first few file names of a porcelain status, for a one-line detail.
fn name_list(porcelain: &str) -> String {
    // `XY path`: the status columns then a space. The first line may have
    // lost its leading column to the git helper's trim, so split on the
    // first space after the columns instead of slicing a fixed width.
    let names: Vec<&str> = porcelain
        .lines()
        .filter_map(|l| l.trim_start().split_once(' '))
        .map(|(_, path)| path.trim())
        .filter(|s| !s.is_empty())
        .collect();
    let shown: Vec<&str> = names.iter().take(5).copied().collect();
    let mut s = shown.join(", ");
    if names.len() > shown.len() {
        s.push_str(&format!(" (+{} more)", names.len() - shown.len()));
    }
    s
}

/// Gather a submission for `card`'s worktree against the project's
/// checkout at `dest_cwd`. Requires a clean, idle worktree on its branch
/// and both trees in the same repository.
pub fn prepare_submission(
    dest_cwd: &Path,
    card: &str,
    wt: &crate::kanban::Worktree,
    worker: Option<String>,
) -> Result<Submission, String> {
    let repo = repo_identity(dest_cwd)?;
    let dest = crate::kanban::git(dest_cwd, &["rev-parse", "--show-toplevel"])?;
    let tree = Path::new(&wt.path);
    let commit = if wt.in_place {
        source_preflight_in_place(tree, &wt.branch, None)
    } else {
        source_preflight(tree, &wt.branch, None)
    }
    .map_err(|(_, d)| d)?;
    let wt_repo = repo_identity(tree)?;
    if wt_repo != repo {
        return Err(format!(
            "worktree {} belongs to another repository ({})",
            wt.path,
            wt_repo.display()
        ));
    }
    Ok(Submission {
        card: card.to_string(),
        repo: repo.to_string_lossy().into_owned(),
        target: wt.base.clone(),
        dest,
        worktree: wt.path.clone(),
        branch: wt.branch.clone(),
        commit,
        worker,
        in_place: wt.in_place,
    })
}

// ---------------------------------------------------------------------------
// Subprocesses: bounded, cancellable, killed with the owner.
// ---------------------------------------------------------------------------

struct ChildOutcome {
    ok: bool,
    tail: String,
    failure_lines: String,
    timed_out: bool,
    cancelled: bool,
}

/// Run `cmd` to completion with a deadline and a cancel probe polled every
/// 100 ms. The child joins a kill-on-close Job, so an owner that dies takes
/// it (and its own children — a `cargo test` tree) along instead of leaving
/// an orphan to overlap the next owner. Output is captured (both streams)
/// and the last [`OUTPUT_TAIL_LINES`] returned. Potential failure lines are
/// also kept separately so a noisy ending cannot bury the useful diagnosis.
fn run_child(
    mut cmd: std::process::Command,
    timeout: Duration,
    cancelled: &dyn Fn() -> bool,
) -> Result<ChildOutcome, String> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    let mut job = crate::job::Job::assign(child.id());
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (out_rx, err_rx) = (read_all(stdout), read_all(stderr));
    let start = Instant::now();
    let mut timed_out = false;
    let mut was_cancelled = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {
                if start.elapsed() >= timeout {
                    timed_out = true;
                } else if cancelled() {
                    was_cancelled = true;
                }
                if timed_out || was_cancelled {
                    // The whole tree first: a descendant of the check (a
                    // test runner's workers, a shell wrapper's child) holds
                    // the output pipes too, and killing only the immediate
                    // process would leave the readers waiting on it.
                    drop(job.take());
                    let _ = child.kill();
                    break child
                        .wait()
                        .map_err(|e| format!("cannot reap {program}: {e}"))?;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(format!("cannot wait for {program}: {e}")),
        }
    };
    // Bounded: a lingering descendant that kept a pipe open must not hold
    // the repository's turn hostage. Whatever arrived is what we report;
    // the Job drop at return kills the straggler.
    let (out, err) = collect_output(&out_rx, &err_rx, OUTPUT_GRACE);
    drop(job);
    let mut combined = out;
    if !err.trim().is_empty() {
        if !combined.is_empty() && !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str(&err);
    }
    Ok(ChildOutcome {
        ok: status.success() && !timed_out && !was_cancelled,
        tail: tail_lines(&combined, OUTPUT_TAIL_LINES),
        failure_lines: failure_lines(&combined),
        timed_out,
        cancelled: was_cancelled,
    })
}

/// How long to wait for a finished child's output after it exited: only a
/// descendant that outlived it can still be writing.
const OUTPUT_GRACE: Duration = Duration::from_secs(5);

/// Drain one captured pipe on its own thread (reading both pipes from one
/// thread can deadlock once either buffer fills). The thread is never
/// joined: it ends when the last holder of the pipe closes it, and the
/// caller waits a bounded time on the channel instead.
fn read_all(
    pipe: Option<impl std::io::Read + Send + 'static>,
) -> std::sync::mpsc::Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(mut p) = pipe {
            let mut bytes = Vec::new();
            let _ = std::io::Read::read_to_end(&mut p, &mut bytes);
            buf = String::from_utf8_lossy(&bytes).into_owned();
        }
        let _ = tx.send(buf);
    });
    rx
}

/// Both pipes share one deadline: a straggler holds stdout and stderr
/// alike, and waiting `grace` on each in turn would double the stall.
fn collect_output(
    out_rx: &std::sync::mpsc::Receiver<String>,
    err_rx: &std::sync::mpsc::Receiver<String>,
    grace: Duration,
) -> (String, String) {
    let deadline = Instant::now() + grace;
    let recv = |rx: &std::sync::mpsc::Receiver<String>| {
        rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_default()
    };
    (recv(out_rx), recv(err_rx))
}

fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n").trim().to_string()
}

fn failure_lines(text: &str) -> String {
    text.lines()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("failed") || lower.contains("panicked at") || lower.contains("error:")
        })
        .take(FAILURE_LINES_MAX)
        .map(|line| {
            line.trim()
                .chars()
                .take(FAILURE_LINE_CHARS)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn check_output(out: &ChildOutcome) -> String {
    if !out.ok && !out.failure_lines.is_empty() {
        format!(
            "Failure lines:\n{}\n\nLast {OUTPUT_TAIL_LINES} lines:\n{}",
            out.failure_lines, out.tail
        )
    } else {
        out.tail.clone()
    }
}

/// A long-lived git command in `cwd` (rebase, merge): bounded, never
/// cancelled mid-way (cancellation is honored between stages so git state
/// is never torn).
fn run_git_long(cwd: &Path, args: &[&str]) -> Result<ChildOutcome, String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(cwd).args(args);
    run_child(cmd, GIT_OP_TIMEOUT, &|| false)
}

// ---------------------------------------------------------------------------
// Check policy.
// ---------------------------------------------------------------------------

fn default_timeout() -> u64 {
    DEFAULT_CHECK_TIMEOUT_SECS
}

/// `<root>/.foreman/integrate.json`: the command every integration runs
/// in the prepared worktree, as argv (no shell), and its timeout.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CheckPolicy {
    #[serde(default)]
    pub v: u32,
    #[serde(default)]
    pub check: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

/// The destination's policy: `Ok(None)` when no file exists or it names
/// no command (checks are not configured, and the record says so); `Err`
/// when the file exists but cannot be read.
pub fn load_policy(root: &Path) -> Result<Option<CheckPolicy>, String> {
    let path = root.join(POLICY_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let policy: CheckPolicy = serde_json::from_str(&text)
        .map_err(|e| format!("{} is not a valid policy: {e}", path.display()))?;
    if policy.check.is_empty() {
        return Ok(None);
    }
    Ok(Some(policy))
}

enum CheckError {
    Cancelled,
    Failed(String),
}

fn run_check(
    tree: &Path,
    policy: Option<&CheckPolicy>,
    prepared: &str,
    target: &str,
    cancelled: &dyn Fn() -> bool,
) -> Result<CheckRecord, CheckError> {
    let Some(p) = policy else {
        return Ok(CheckRecord {
            command: Vec::new(),
            ok: true,
            tail: "no checks configured".into(),
            prepared: prepared.to_string(),
            target: target.to_string(),
        });
    };
    let mut cmd = std::process::Command::new(&p.check[0]);
    cmd.args(&p.check[1..]).current_dir(tree);
    let out = run_child(cmd, Duration::from_secs(p.timeout_secs), cancelled)
        .map_err(CheckError::Failed)?;
    if out.cancelled {
        return Err(CheckError::Cancelled);
    }
    let detail = check_output(&out);
    let tail = if out.timed_out {
        format!("timed out after {} s\n{detail}", p.timeout_secs)
    } else {
        detail
    };
    Ok(CheckRecord {
        command: p.check.clone(),
        ok: out.ok,
        tail,
        prepared: prepared.to_string(),
        target: target.to_string(),
    })
}

// ---------------------------------------------------------------------------
// The turn.
// ---------------------------------------------------------------------------

/// Test barriers inside a turn; production passes [`NoHooks`].
pub trait TurnHooks {
    fn after_rebase(&self, _card: &str) {}
    fn after_check(&self, _card: &str) {}
    fn before_merge(&self, _card: &str) {}
}

pub struct NoHooks;

impl TurnHooks for NoHooks {}

/// What one turn reported, per request, for toasts and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEvent {
    Integrated {
        card: String,
        prepared: String,
    },
    NeedsResolution {
        card: String,
        reason: Reason,
        detail: String,
    },
    /// Still queued; the destination refused. The turn stops here.
    Held {
        card: String,
        detail: String,
    },
    Cancelled {
        card: String,
        note: String,
    },
    /// Put back to the queue after bounded retries.
    Requeued {
        card: String,
        note: String,
    },
    Recovered {
        card: String,
        note: String,
    },
}

/// The destination is on `target`, idle, and clean outside `.foreman/`
/// (the app writes card files there); returns its HEAD.
fn dest_preflight(dest: &Path, target: &str) -> Result<String, (Reason, String)> {
    if !dest.is_dir() {
        return Err((
            Reason::ProcessFailed,
            format!("destination checkout {} is missing", dest.display()),
        ));
    }
    let on = crate::kanban::git(dest, &["symbolic-ref", "--short", "HEAD"]).map_err(|_| {
        (
            Reason::DestinationBranch,
            format!("destination checkout has no branch checked out; check out {target}"),
        )
    })?;
    if on != target {
        return Err((
            Reason::DestinationBranch,
            format!("destination checkout is on {on}, expected {target}"),
        ));
    }
    if let Some(op) = git_op_in_progress(dest) {
        return Err((
            Reason::DestinationBusy,
            format!("a git {op} is in progress in the destination checkout"),
        ));
    }
    let dirty = crate::kanban::git(
        dest,
        &[
            "status",
            "--porcelain",
            "--untracked-files=no",
            "--",
            ".",
            ":(exclude).foreman",
        ],
    )
    .map_err(|e| (Reason::ProcessFailed, e))?;
    if !dirty.trim().is_empty() {
        return Err((
            Reason::DestinationDirty,
            format!(
                "uncommitted changes in the destination checkout: {}",
                name_list(&dirty)
            ),
        ));
    }
    crate::kanban::git(dest, &["rev-parse", "HEAD"]).map_err(|e| (Reason::ProcessFailed, e))
}

enum RebaseError {
    Conflict(String),
    Other(String),
}

/// `git rebase <onto>` in the worktree. A stop with conflicts leaves the
/// rebase in progress for the worker; any other failure is reported.
fn rebase(tree: &Path, onto: &str) -> Result<(), RebaseError> {
    let out = run_git_long(tree, &["rebase", onto]).map_err(RebaseError::Other)?;
    if out.ok {
        return Ok(());
    }
    if git_op_in_progress(tree).is_some() {
        let files = crate::kanban::git(tree, &["diff", "--name-only", "--diff-filter=U"])
            .unwrap_or_default();
        let names: Vec<&str> = files
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        return Err(RebaseError::Conflict(if names.is_empty() {
            format!("rebase stopped: {}", first_line(&out.tail))
        } else {
            format!("conflicts in: {}", names.join(", "))
        }));
    }
    Err(RebaseError::Other(if out.timed_out {
        "rebase timed out".into()
    } else {
        out.tail
    }))
}

enum FfError {
    Moved,
    Dirty(String),
    Locked(String),
    Other(String),
}

/// `git merge --ff-only <prepared>` in the destination, classified by git's
/// own refusal text so the caller can retry (moved, locked), hold (dirty),
/// or hand back (anything else).
fn fast_forward(dest: &Path, prepared: &str) -> Result<(), FfError> {
    let out = run_git_long(dest, &["merge", "--ff-only", prepared]).map_err(FfError::Other)?;
    if out.ok {
        return Ok(());
    }
    let lower = out.tail.to_lowercase();
    if lower.contains("not possible to fast-forward") || lower.contains("cannot fast-forward") {
        Err(FfError::Moved)
    } else if lower.contains("would be overwritten") {
        Err(FfError::Dirty(out.tail))
    } else if lower.contains(".lock") {
        Err(FfError::Locked(out.tail))
    } else {
        Err(FfError::Other(out.tail))
    }
}

fn attempt_for(owner: &Owner) -> Attempt {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(owner.run.as_bytes());
    h.update(owner.pid.to_le_bytes());
    h.update(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
            .to_le_bytes(),
    );
    let id: String = h
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .take(12)
        .collect();
    Attempt {
        id,
        pid: owner.pid,
        run: owner.run.clone(),
        started: crate::kanban::now_stamp(),
        stage: Stage::Preflight,
        retries: 0,
        target_commit: None,
        prepared: None,
        check: None,
    }
}

fn set_outcome(req: &mut Request, reason: Reason, detail: String) {
    req.outcome = Some(Outcome {
        reason,
        detail,
        next: reason.next_action().to_string(),
        at: crate::kanban::now_stamp(),
    });
}

/// Hand the request back to the worker.
fn resolve(mut req: Request, reason: Reason, detail: String) -> (Request, TurnEvent) {
    req.phase = Phase::NeedsResolution;
    req.hold = None;
    set_outcome(&mut req, reason, detail.clone());
    let card = req.card.clone();
    (
        req,
        TurnEvent::NeedsResolution {
            card,
            reason,
            detail,
        },
    )
}

/// Keep the request queued; the destination refused this turn.
fn hold(mut req: Request, reason: Reason, detail: String) -> (Request, TurnEvent) {
    req.phase = Phase::Queued;
    req.attempt = None;
    req.outcome = None;
    let text = format!("{}: {detail}", reason.label());
    req.hold = Some(text.clone());
    let card = req.card.clone();
    (req, TurnEvent::Held { card, detail: text })
}

fn requeue(mut req: Request, note: String) -> (Request, TurnEvent) {
    req.phase = Phase::Queued;
    req.attempt = None;
    req.outcome = None;
    req.hold = None;
    req.note = Some(note.clone());
    let card = req.card.clone();
    (req, TurnEvent::Requeued { card, note })
}

fn cancelled(req: Request, note: String) -> (Request, TurnEvent) {
    let card = req.card.clone();
    (req, TurnEvent::Cancelled { card, note })
}

/// One request's turn: preflight, rebase, check, final preflight,
/// fast-forward — persisting the stage before each step. Cancel checkpoints
/// sit between stages and inside the check runner.
fn integrate_one(queue: &Queue, mut req: Request, hooks: &dyn TurnHooks) -> (Request, TurnEvent) {
    if req.in_place {
        return integrate_in_place(queue, req, hooks);
    }
    let card = req.card.clone();
    let tree = PathBuf::from(&req.worktree);
    let dest = PathBuf::from(&req.dest);
    let is_cancelled = || queue.cancel_pending(&card);
    macro_rules! save {
        () => {
            if let Err(e) = queue.save_progress(&mut req) {
                return resolve(req, Reason::ProcessFailed, e);
            }
        };
    }
    macro_rules! stage {
        ($s:expr) => {
            if let Some(a) = req.attempt.as_mut() {
                a.stage = $s;
            }
            save!();
        };
    }

    if let Err((reason, detail)) = source_preflight(&tree, &req.branch, Some(&req.commit)) {
        return resolve(req, reason, detail);
    }
    let mut target_commit = match dest_preflight(&dest, &req.target) {
        Ok(c) => c,
        Err((reason, detail)) => return hold(req, reason, detail),
    };
    let mut retries = 0u32;
    loop {
        if is_cancelled() {
            return cancelled(
                req,
                "cancelled before the rebase; worktree untouched".into(),
            );
        }
        // Rebase onto the exact target commit the check will be tied to.
        if let Some(a) = req.attempt.as_mut() {
            a.target_commit = Some(target_commit.clone());
            a.retries = retries;
            a.prepared = None;
            a.check = None;
        }
        stage!(Stage::Rebase);
        match rebase(&tree, &target_commit) {
            Ok(()) => {}
            Err(RebaseError::Conflict(files)) => return resolve(req, Reason::Conflict, files),
            Err(RebaseError::Other(e)) => return resolve(req, Reason::ProcessFailed, e),
        }
        let prepared = match crate::kanban::git(&tree, &["rev-parse", "HEAD"]) {
            Ok(c) => c,
            Err(e) => return resolve(req, Reason::ProcessFailed, e),
        };
        if let Some(a) = req.attempt.as_mut() {
            a.prepared = Some(prepared.clone());
        }
        save!();
        hooks.after_rebase(&card);
        if is_cancelled() {
            return cancelled(
                req,
                format!(
                    "cancelled after the rebase; worktree is at {}",
                    short(&prepared)
                ),
            );
        }
        // Checks, against the prepared commit and this target.
        stage!(Stage::Check);
        let policy = match load_policy(&dest) {
            Ok(p) => p,
            Err(e) => return resolve(req, Reason::ProcessFailed, e),
        };
        match run_check(
            &tree,
            policy.as_ref(),
            &prepared,
            &target_commit,
            &is_cancelled,
        ) {
            Ok(rec) => {
                let ok = rec.ok;
                let tail = rec.tail.clone();
                if let Some(a) = req.attempt.as_mut() {
                    a.check = Some(rec);
                }
                if !ok {
                    return resolve(req, Reason::CheckFailed, tail);
                }
            }
            Err(CheckError::Cancelled) => {
                return cancelled(
                    req,
                    format!(
                        "cancelled during the check; worktree is at {}",
                        short(&prepared)
                    ),
                );
            }
            Err(CheckError::Failed(e)) => return resolve(req, Reason::ProcessFailed, e),
        }
        save!();
        hooks.after_check(&card);
        if is_cancelled() {
            return cancelled(
                req,
                format!(
                    "cancelled after the check; worktree is at {}",
                    short(&prepared)
                ),
            );
        }
        // Final preflight: same source, same target, clean destination.
        if let Err((reason, detail)) = source_preflight(&tree, &req.branch, Some(&prepared)) {
            return resolve(req, reason, detail);
        }
        let now_target = match dest_preflight(&dest, &req.target) {
            Ok(c) => c,
            Err((reason, detail)) => return hold(req, reason, detail),
        };
        if now_target != target_commit {
            retries += 1;
            if retries > MAX_TARGET_RETRIES {
                let note = format!(
                    "{} moved {retries} times during the turn; queued again",
                    req.target
                );
                return requeue(req, note);
            }
            target_commit = now_target;
            continue;
        }
        if is_cancelled() {
            return cancelled(
                req,
                format!(
                    "cancelled before the fast-forward; worktree is at {}",
                    short(&prepared)
                ),
            );
        }
        // The prepared commit and target are on disk before the target
        // moves: recovery can tell "merged" from "about to merge".
        stage!(Stage::Merge);
        hooks.before_merge(&card);
        let mut lock_retries = 0u32;
        loop {
            match fast_forward(&dest, &prepared) {
                Ok(()) => {
                    match crate::kanban::git(&dest, &["rev-parse", "HEAD"]) {
                        Ok(h) if h == prepared => {}
                        Ok(h) => {
                            let detail = format!(
                                "fast-forward reported success but {} is at {}",
                                req.target,
                                short(&h)
                            );
                            return resolve(req, Reason::ProcessFailed, detail);
                        }
                        Err(e) => return resolve(req, Reason::ProcessFailed, e),
                    }
                    req.phase = Phase::Integrated;
                    req.outcome = None;
                    req.hold = None;
                    let card = req.card.clone();
                    return (req, TurnEvent::Integrated { card, prepared });
                }
                Err(FfError::Moved) => {
                    retries += 1;
                    if retries > MAX_TARGET_RETRIES {
                        let note = format!(
                            "{} moved {retries} times during the turn; queued again",
                            req.target
                        );
                        return requeue(req, note);
                    }
                    target_commit = match crate::kanban::git(&dest, &["rev-parse", "HEAD"]) {
                        Ok(c) => c,
                        Err(e) => return resolve(req, Reason::ProcessFailed, e),
                    };
                    break; // re-prepare and re-validate against the new target
                }
                Err(FfError::Dirty(msg)) => return hold(req, Reason::DestinationDirty, msg),
                Err(FfError::Locked(msg)) => {
                    lock_retries += 1;
                    if lock_retries > MAX_TARGET_RETRIES {
                        return requeue(
                            req,
                            format!("git lock contention in the destination: {msg}; queued again"),
                        );
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
                Err(FfError::Other(e)) => return resolve(req, Reason::ProcessFailed, e),
            }
        }
    }
}

/// Branch mode's turn (spec: dispatch-branch §Integration): preflight,
/// ancestry, check, final preflight, then a ref-only fast-forward. The
/// checkout is never rebased or switched with files in flight:
///
/// - a target that is not an ancestor of the branch goes back to the worker
///   (`base moved`) — rebasing a shared checkout could strand the human's
///   uncommitted changes mid-conflict;
/// - the target moves by `git update-ref <target> <commit> <old>`, a
///   compare-and-swap that fails if anyone moved it meanwhile;
/// - HEAD then moves to the target by `git symbolic-ref`. Target and branch
///   are the same commit at that point, so the index and every file,
///   committed or not, are already right: nothing on disk changes.
///
/// The check runs in the checkout as it stands, uncommitted changes and all.
fn integrate_in_place(
    queue: &Queue,
    mut req: Request,
    hooks: &dyn TurnHooks,
) -> (Request, TurnEvent) {
    let card = req.card.clone();
    let tree = PathBuf::from(&req.worktree);
    let target_ref = format!("refs/heads/{}", req.target);
    let commit = req.commit.clone();
    let is_cancelled = || queue.cancel_pending(&card);
    macro_rules! save {
        () => {
            if let Err(e) = queue.save_progress(&mut req) {
                return resolve(req, Reason::ProcessFailed, e);
            }
        };
    }
    macro_rules! stage {
        ($s:expr) => {
            if let Some(a) = req.attempt.as_mut() {
                a.stage = $s;
            }
            save!();
        };
    }

    let mut retries = 0u32;
    'turn: loop {
        if let Err((reason, detail)) = source_preflight_in_place(&tree, &req.branch, Some(&commit))
        {
            return resolve(req, reason, detail);
        }
        let target_commit = match target_preflight(&tree, &req.target) {
            Ok(c) => c,
            Err((reason, detail)) => return hold(req, reason, detail),
        };
        if crate::kanban::git(
            &tree,
            &["merge-base", "--is-ancestor", &target_commit, &commit],
        )
        .is_err()
        {
            let detail = format!(
                "{} moved to {} and is no longer an ancestor of {}",
                req.target,
                short(&target_commit),
                req.branch
            );
            return resolve(req, Reason::BaseMoved, detail);
        }
        if is_cancelled() {
            return cancelled(req, "cancelled before the check; checkout untouched".into());
        }
        if let Some(a) = req.attempt.as_mut() {
            a.target_commit = Some(target_commit.clone());
            a.retries = retries;
            a.prepared = Some(commit.clone());
            a.check = None;
        }
        stage!(Stage::Check);
        let policy = match load_policy(&tree) {
            Ok(p) => p,
            Err(e) => return resolve(req, Reason::ProcessFailed, e),
        };
        match run_check(
            &tree,
            policy.as_ref(),
            &commit,
            &target_commit,
            &is_cancelled,
        ) {
            Ok(rec) => {
                let ok = rec.ok;
                let tail = rec.tail.clone();
                if let Some(a) = req.attempt.as_mut() {
                    a.check = Some(rec);
                }
                if !ok {
                    return resolve(req, Reason::CheckFailed, tail);
                }
            }
            Err(CheckError::Cancelled) => {
                return cancelled(req, "cancelled during the check; checkout untouched".into());
            }
            Err(CheckError::Failed(e)) => return resolve(req, Reason::ProcessFailed, e),
        }
        save!();
        hooks.after_check(&card);
        if is_cancelled() {
            return cancelled(req, "cancelled after the check; checkout untouched".into());
        }
        // Final preflight: same branch commit, same target.
        if let Err((reason, detail)) = source_preflight_in_place(&tree, &req.branch, Some(&commit))
        {
            return resolve(req, reason, detail);
        }
        match target_preflight(&tree, &req.target) {
            Ok(c) if c == target_commit => {}
            Ok(_) => {
                retries += 1;
                if retries > MAX_TARGET_RETRIES {
                    let note = format!(
                        "{} moved {retries} times during the turn; queued again",
                        req.target
                    );
                    return requeue(req, note);
                }
                continue 'turn;
            }
            Err((reason, detail)) => return hold(req, reason, detail),
        }
        stage!(Stage::Merge);
        hooks.before_merge(&card);
        let msg = format!("foreman: integrate {}", req.branch);
        let mut lock_retries = 0u32;
        loop {
            match crate::kanban::git(
                &tree,
                &[
                    "update-ref",
                    "-m",
                    &msg,
                    &target_ref,
                    &commit,
                    &target_commit,
                ],
            ) {
                Ok(_) => break,
                Err(e) if e.contains(".lock") && lock_retries < MAX_TARGET_RETRIES => {
                    lock_retries += 1;
                    std::thread::sleep(Duration::from_millis(500));
                }
                Err(e) => {
                    let now = crate::kanban::git(&tree, &["rev-parse", &target_ref]);
                    if now.as_deref().is_ok_and(|n| n != target_commit) {
                        retries += 1;
                        if retries > MAX_TARGET_RETRIES {
                            let note = format!(
                                "{} moved {retries} times during the turn; queued again",
                                req.target
                            );
                            return requeue(req, note);
                        }
                        continue 'turn;
                    }
                    return resolve(req, Reason::ProcessFailed, e);
                }
            }
        }
        // The target now holds the branch's commit. Put the checkout on
        // it: a pure HEAD move, since both refs name the same commit.
        if let Err(e) = crate::kanban::git(&tree, &["symbolic-ref", "HEAD", &target_ref]) {
            let detail = format!(
                "{} was fast-forwarded to {} but the checkout could not be moved onto it ({e}); run git switch {}",
                req.target,
                short(&commit),
                req.target
            );
            return resolve(req, Reason::ProcessFailed, detail);
        }
        match crate::kanban::git(&tree, &["rev-parse", "HEAD"]) {
            Ok(h) if h == commit => {}
            Ok(h) => {
                let detail = format!(
                    "fast-forward reported success but {} is at {}",
                    req.target,
                    short(&h)
                );
                return resolve(req, Reason::ProcessFailed, detail);
            }
            Err(e) => return resolve(req, Reason::ProcessFailed, e),
        }
        req.phase = Phase::Integrated;
        req.outcome = None;
        req.hold = None;
        let card = req.card.clone();
        return (
            req,
            TurnEvent::Integrated {
                card,
                prepared: commit,
            },
        );
    }
}

/// Recovery for a branch-mode request a dead owner left `integrating`.
/// Landed (the commit is on the target) → integrated, and a checkout still
/// on the card's branch at that same commit is moved onto the target the
/// way the turn would have; otherwise nothing was written that the next
/// turn's preflight cannot judge, so it is queued again.
fn recover_in_place(req: &mut Request) -> String {
    let tree = PathBuf::from(&req.worktree);
    let target_ref = format!("refs/heads/{}", req.target);
    let landed = crate::kanban::git(
        &tree,
        &["merge-base", "--is-ancestor", &req.commit, &target_ref],
    )
    .is_ok();
    if !landed {
        req.phase = Phase::Queued;
        req.outcome = None;
        req.attempt = None;
        return "recovered before the fast-forward; queued again".into();
    }
    req.phase = Phase::Integrated;
    req.outcome = None;
    let on = crate::kanban::git(&tree, &["symbolic-ref", "-q", "HEAD"]).unwrap_or_default();
    let head = crate::kanban::git(&tree, &["rev-parse", "HEAD"]).unwrap_or_default();
    let target = crate::kanban::git(&tree, &["rev-parse", &target_ref]).unwrap_or_default();
    if on == format!("refs/heads/{}", req.branch) && !head.is_empty() && head == target {
        let _ = crate::kanban::git(&tree, &["symbolic-ref", "HEAD", &target_ref]);
    }
    format!(
        "recovered: {} is already on {}",
        short(&req.commit),
        req.target
    )
}

/// Requests a dead owner left `integrating`: decide from git history and
/// operation state, never from a missing reply. Runs under the turn lock,
/// so "integrating" with the lock free means the owner is gone.
fn recover(queue: &Queue) -> Result<Vec<TurnEvent>, String> {
    let mut events = Vec::new();
    for mut req in queue.requests() {
        if req.phase != Phase::Integrating {
            continue;
        }
        let tree = PathBuf::from(&req.worktree);
        let dest = PathBuf::from(&req.dest);
        let prepared = req.attempt.as_ref().and_then(|a| a.prepared.clone());
        if req.in_place {
            let note = recover_in_place(&mut req);
            req.hold = None;
            req.note = Some(note.clone());
            {
                let _m = queue.meta()?;
                req.touch();
                queue.write(&req)?;
            }
            events.push(TurnEvent::Recovered {
                card: req.card.clone(),
                note,
            });
            continue;
        }
        let landed = prepared.as_deref().is_some_and(|p| {
            crate::kanban::git(&dest, &["merge-base", "--is-ancestor", p, &req.target]).is_ok()
        });
        let note = if landed {
            let p = prepared.clone().unwrap_or_default();
            req.phase = Phase::Integrated;
            req.outcome = None;
            format!(
                "recovered: prepared commit {} is already on {}",
                short(&p),
                req.target
            )
        } else if !tree.join(".git").exists() {
            req.phase = Phase::NeedsResolution;
            set_outcome(
                &mut req,
                Reason::SourceMissing,
                format!("worktree {} is missing", tree.display()),
            );
            "recovered: worktree missing".to_string()
        } else if git_op_in_progress(&tree).is_some() {
            req.phase = Phase::NeedsResolution;
            set_outcome(
                &mut req,
                Reason::Interrupted,
                "integration was interrupted while a rebase was in progress".into(),
            );
            "recovered: rebase left in progress by a dead owner".to_string()
        } else {
            match crate::kanban::git(&tree, &["rev-parse", "HEAD"]) {
                Ok(h) if h == req.commit => {
                    req.phase = Phase::Queued;
                    req.outcome = None;
                    "recovered before the rebase started; queued again".to_string()
                }
                Ok(h) if Some(&h) == prepared.as_ref() => {
                    req.commit = h;
                    req.phase = Phase::Queued;
                    req.outcome = None;
                    "recovered after the rebase; queued again from the prepared commit".to_string()
                }
                Ok(h) => {
                    req.phase = Phase::NeedsResolution;
                    set_outcome(
                        &mut req,
                        Reason::SourceChanged,
                        format!(
                            "worktree moved to {} while integration was interrupted",
                            short(&h)
                        ),
                    );
                    "recovered: worktree moved".to_string()
                }
                Err(e) => {
                    req.phase = Phase::NeedsResolution;
                    set_outcome(&mut req, Reason::ProcessFailed, e);
                    "recovered: worktree unreadable".to_string()
                }
            }
        };
        if req.phase != Phase::Integrated {
            req.attempt = None;
        }
        req.hold = None;
        req.note = Some(note.clone());
        {
            let _m = queue.meta()?;
            req.touch();
            queue.write(&req)?;
        }
        events.push(TurnEvent::Recovered {
            card: req.card.clone(),
            note,
        });
    }
    Ok(events)
}

/// Take the repository's turn if it is free, recover anything a dead owner
/// left, then integrate queued requests in submission order until the
/// queue is empty or the destination refuses (a hold stops the turn:
/// every card behind it would hit the same wall). `Ok(None)` = another
/// owner holds the turn; nothing was touched.
pub fn run_turn(
    queue: &Queue,
    owner: &Owner,
    hooks: &dyn TurnHooks,
) -> Result<Option<Vec<TurnEvent>>, String> {
    let Some(_turn) = queue.try_turn()? else {
        return Ok(None);
    };
    let mut events = recover(queue)?;
    // After a hold, only branch-mode requests are still tried this turn,
    // each at most once: a worktree card held because the checkout is on a
    // branch-mode card's branch must not keep that card from integrating
    // and putting the checkout back (spec: dispatch-branch §Integration).
    let mut held: Vec<String> = Vec::new();
    loop {
        let Some(mut req) = queue.requests().into_iter().find(|r| {
            r.phase == Phase::Queued && (held.is_empty() || (r.in_place && !held.contains(&r.card)))
        }) else {
            break;
        };
        {
            let _m = queue.meta()?;
            // A cancel that landed between the read and the lock wins.
            // A cancel (file gone) or a resubmission (new seq/commit) that
            // landed between the read and the lock wins: work from the
            // file, never from the stale copy.
            match queue.get(&req.card) {
                Some(cur) if cur.phase == Phase::Queued => req = cur,
                _ => continue,
            }
            req.phase = Phase::Integrating;
            req.attempt = Some(attempt_for(owner));
            req.outcome = None;
            req.hold = None;
            req.note = None;
            req.touch();
            queue.write(&req)?;
        }
        let req_card = req.card.clone();
        let (req, mut event) = integrate_one(queue, req, hooks);
        {
            let _m = queue.meta()?;
            let mut req = req;
            req.touch();
            // A cancel that arrived on disk after the last checkpoint (the
            // final preflight has none) is honored here for anything that
            // did not land: the request would otherwise survive as queued
            // or handed back with the flag silently erased.
            let cancel_on_disk = queue.get(&req.card).is_none_or(|r| r.cancel_requested);
            let landed = req.phase == Phase::Integrated;
            if cancel_on_disk && !landed && !matches!(event, TurnEvent::Cancelled { .. }) {
                let prepared = req
                    .attempt
                    .as_ref()
                    .and_then(|a| a.prepared.as_deref())
                    .map(|p| format!("; worktree is at {}", short(p)))
                    .unwrap_or_default();
                event = TurnEvent::Cancelled {
                    card: req.card.clone(),
                    note: format!("cancelled while {}{prepared}", req.phase.label()),
                };
            }
            match &event {
                TurnEvent::Cancelled { .. } => queue.remove_file(&req.card)?,
                _ => {
                    req.cancel_requested = false;
                    queue.write(&req)?;
                }
            }
        }
        let is_held = matches!(event, TurnEvent::Held { .. });
        let stop = matches!(event, TurnEvent::Requeued { .. });
        if is_held {
            held.push(req_card);
        }
        events.push(event);
        if stop {
            break;
        }
    }
    Ok(Some(events))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_in(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// `git init -b main` with one tracked file; `None` skips when git is
    /// not on PATH (never a failure).
    fn repo() -> Option<tempfile::TempDir> {
        if !crate::kanban::git_available() {
            eprintln!("git not on PATH; skipping");
            return None;
        }
        let tmp = tempfile::tempdir().unwrap();
        git_in(tmp.path(), &["init", "-q", "-b", "main"]);
        // In the repo's own config, not `-c`: the queue's own git calls
        // (rebase) commit too, and a CI runner has no global identity.
        for (k, v) in [
            ("user.name", "t"),
            ("user.email", "t@t"),
            ("commit.gpgsign", "false"),
        ] {
            git_in(tmp.path(), &["config", k, v]);
        }
        std::fs::write(tmp.path().join("f.txt"), "one\n").unwrap();
        git_in(tmp.path(), &["add", "f.txt"]);
        git_in(tmp.path(), &["commit", "-q", "-m", "init"]);
        Some(tmp)
    }

    fn head(dir: &Path) -> String {
        git_in(dir, &["rev-parse", "HEAD"])
    }

    fn bring_up(repo: &Path, id: &str) -> crate::kanban::Worktree {
        let card =
            crate::kanban::Card::new(id.into(), "t".into(), None, crate::kanban::now_stamp());
        match crate::kanban::bring_up_worktree(repo, &card).unwrap() {
            crate::kanban::BringUp::Worktree(wt) => wt,
            other => panic!("expected a worktree, got {other:?}"),
        }
    }

    fn commit_file(tree: &Path, name: &str, content: &str, msg: &str) -> String {
        std::fs::write(tree.join(name), content).unwrap();
        git_in(tree, &["add", name]);
        git_in(tree, &["commit", "-q", "-m", msg]);
        head(tree)
    }

    fn submit(repo: &Path, id: &str, wt: &crate::kanban::Worktree) -> (Queue, Submitted) {
        let sub = prepare_submission(repo, id, wt, Some("t1".into())).unwrap();
        let queue = Queue::for_repo(Path::new(&sub.repo));
        let s = queue.submit(sub).unwrap();
        (queue, s)
    }

    fn owner() -> Owner {
        Owner::this_process()
    }

    fn turn(queue: &Queue) -> Vec<TurnEvent> {
        run_turn(queue, &owner(), &NoHooks)
            .unwrap()
            .expect("the turn was free")
    }

    fn write_policy(root: &Path, check: &[&str]) {
        std::fs::create_dir_all(root.join(".foreman")).unwrap();
        let policy = CheckPolicy {
            v: 1,
            check: check.iter().map(|s| s.to_string()).collect(),
            timeout_secs: 60,
        };
        std::fs::write(
            root.join(POLICY_FILE),
            serde_json::to_string(&policy).unwrap(),
        )
        .unwrap();
    }

    // -- pure ---------------------------------------------------------------

    fn sample_request(card: &str, seq: u64, phase: Phase) -> Request {
        Request {
            v: REQUEST_V,
            seq,
            card: card.into(),
            repo: "C:/r/.git".into(),
            target: "main".into(),
            dest: "C:/r".into(),
            worktree: format!("C:/r/.foreman/worktrees/{card}"),
            branch: format!("card/{card}"),
            commit: "0123456789abcdef".into(),
            worker: None,
            submitted: "2026-09-17T00:00:00Z".into(),
            updated: "2026-09-17T00:00:00Z".into(),
            phase,
            attempt: None,
            outcome: None,
            hold: None,
            cancel_requested: false,
            note: None,
            in_place: false,
        }
    }

    #[test]
    fn failed_check_puts_bounded_failure_lines_before_unchanged_tail() {
        let output = format!(
            "test queue::reports_name ... FAILED\nthread 'queue' panicked at src/queue.rs:12\nerror: check exited\n{}",
            (0..50)
                .map(|n| format!("warning {n}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let tail = tail_lines(&output, OUTPUT_TAIL_LINES);
        assert!(!tail.contains("reports_name"));
        let out = ChildOutcome {
            ok: false,
            tail: tail.clone(),
            failure_lines: failure_lines(&output),
            timed_out: false,
            cancelled: false,
        };
        let detail = check_output(&out);
        assert!(detail.starts_with("Failure lines:\ntest queue::reports_name ... FAILED\n"));
        assert!(detail.contains("panicked at src/queue.rs:12\nerror: check exited"));
        assert!(detail.ends_with(&format!("Last {OUTPUT_TAIL_LINES} lines:\n{tail}")));
    }

    #[test]
    fn failure_excerpt_limits_lines_and_width_and_success_keeps_only_tail() {
        let output = (0..20)
            .map(|n| format!("error: {n} {}", "x".repeat(500)))
            .collect::<Vec<_>>()
            .join("\n");
        let excerpt = failure_lines(&output);
        assert_eq!(excerpt.lines().count(), FAILURE_LINES_MAX);
        assert!(
            excerpt
                .lines()
                .all(|line| line.chars().count() == FAILURE_LINE_CHARS)
        );
        let out = ChildOutcome {
            ok: true,
            tail: "ordinary output".into(),
            failure_lines: excerpt,
            timed_out: false,
            cancelled: false,
        };
        assert_eq!(check_output(&out), "ordinary output");
    }

    #[test]
    fn views_number_queued_requests_in_submission_order_only() {
        let reqs = vec![
            sample_request("c", 3, Phase::Queued),
            sample_request("a", 1, Phase::NeedsResolution),
            sample_request("b", 2, Phase::Queued),
            sample_request("d", 4, Phase::Integrating),
        ];
        let v = views(&reqs);
        assert_eq!(v["b"].position, Some(1));
        assert_eq!(v["c"].position, Some(2));
        assert_eq!(v["a"].position, None);
        assert_eq!(v["d"].position, None);
        assert_eq!(v["b"].summary(), "queued #1");
        assert_eq!(v["a"].summary(), "needs resolution");
        assert_eq!(v["d"].summary(), "integrating");
        assert_eq!(v["b"].tail(), "[integrate queued #1]");
    }

    #[test]
    fn view_summary_carries_stage_reason_hold_and_cancel() {
        let mut r = sample_request("a", 1, Phase::Integrating);
        r.attempt = Some(attempt_for(&owner()));
        r.attempt.as_mut().unwrap().stage = Stage::Check;
        r.cancel_requested = true;
        let v = IntegrationView::from_request(&r, None);
        assert_eq!(v.summary(), "integrating · check · cancelling");

        let mut r = sample_request("a", 1, Phase::NeedsResolution);
        set_outcome(&mut r, Reason::Conflict, "conflicts in: f.txt\nmore".into());
        let v = IntegrationView::from_request(&r, None);
        assert_eq!(
            v.summary(),
            "needs resolution · conflict: conflicts in: f.txt"
        );
        assert_eq!(v.next.as_deref(), Some(Reason::Conflict.next_action()));

        let mut r = sample_request("a", 1, Phase::Queued);
        r.hold =
            Some("destination dirty: uncommitted changes in the destination checkout: x".into());
        let v = IntegrationView::from_request(&r, Some(1));
        assert_eq!(
            v.summary(),
            "queued #1 · held: destination dirty: uncommitted changes in the destination checkout: x"
        );
    }

    #[test]
    fn view_wire_shape_omits_unset_fields_and_round_trips() {
        let r = sample_request("a", 1, Phase::Queued);
        let v = IntegrationView::from_request(&r, Some(2));
        let j = serde_json::to_string(&v).unwrap();
        assert_eq!(
            j,
            r#"{"phase":"queued","commit":"0123456789abcdef","position":2}"#
        );
        let back: IntegrationView = serde_json::from_str(&j).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn request_file_round_trips_and_a_v1_file_without_optional_fields_parses() {
        let mut r = sample_request("a", 7, Phase::Integrating);
        r.attempt = Some(attempt_for(&owner()));
        r.attempt.as_mut().unwrap().check = Some(CheckRecord {
            command: vec!["cargo".into(), "test".into()],
            ok: false,
            tail: "boom".into(),
            prepared: "p".into(),
            target: "t".into(),
        });
        let j = serde_json::to_string(&r).unwrap();
        let back: Request = serde_json::from_str(&j).unwrap();
        assert_eq!(back, r);
        let minimal = r#"{"v":1,"seq":1,"card":"a","repo":"r","target":"main","dest":"d","worktree":"w","branch":"card/a","commit":"c","submitted":"s","updated":"u","phase":"queued"}"#;
        let m: Request = serde_json::from_str(minimal).unwrap();
        assert!(m.attempt.is_none() && m.outcome.is_none() && !m.cancel_requested);
    }

    #[test]
    fn policy_absent_or_empty_means_no_checks_and_garbage_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(load_policy(tmp.path()).unwrap(), None);
        write_policy(tmp.path(), &[]);
        assert_eq!(load_policy(tmp.path()).unwrap(), None);
        write_policy(tmp.path(), &["cargo", "test"]);
        let p = load_policy(tmp.path()).unwrap().unwrap();
        assert_eq!(p.check, vec!["cargo", "test"]);
        std::fs::write(tmp.path().join(POLICY_FILE), "{ nope").unwrap();
        assert!(load_policy(tmp.path()).is_err());
        std::fs::write(tmp.path().join(POLICY_FILE), r#"{"check":["x"]}"#).unwrap();
        assert_eq!(
            load_policy(tmp.path()).unwrap().unwrap().timeout_secs,
            DEFAULT_CHECK_TIMEOUT_SECS
        );
    }

    // -- queue --------------------------------------------------------------

    fn plain_submission(card: &str, commit: &str) -> Submission {
        Submission {
            card: card.into(),
            repo: "C:/r/.git".into(),
            target: "main".into(),
            dest: "C:/r".into(),
            worktree: format!("C:/r/.foreman/worktrees/{card}"),
            branch: format!("card/{card}"),
            commit: commit.into(),
            worker: None,
            in_place: false,
        }
    }

    #[test]
    fn submit_orders_by_seq_dedups_same_commit_and_resubmits_at_the_tail() {
        let tmp = tempfile::tempdir().unwrap();
        let q = Queue::at(tmp.path().join("q"));
        let a = q.submit(plain_submission("a", "c1")).unwrap();
        let b = q.submit(plain_submission("b", "c1")).unwrap();
        assert!(!a.existing && !b.existing);
        assert!(a.request.seq < b.request.seq);
        // same card + commit while queued: the existing request comes back
        let again = q.submit(plain_submission("a", "c1")).unwrap();
        assert!(again.existing);
        assert_eq!(again.request.seq, a.request.seq);
        // a new commit for a queued card replaces it at the tail
        let moved = q.submit(plain_submission("a", "c2")).unwrap();
        assert!(!moved.existing);
        assert!(moved.request.seq > b.request.seq);
        let v = q.views();
        assert_eq!(v["b"].position, Some(1));
        assert_eq!(v["a"].position, Some(2));
        // removal never frees a number
        q.remove("a").unwrap();
        let back = q.submit(plain_submission("a", "c2")).unwrap();
        assert!(back.request.seq > moved.request.seq);
    }

    #[test]
    fn submit_refuses_a_new_commit_while_integrating_and_cancel_flags_it() {
        let tmp = tempfile::tempdir().unwrap();
        let q = Queue::at(tmp.path().join("q"));
        let mut r = q.submit(plain_submission("a", "c1")).unwrap().request;
        r.phase = Phase::Integrating;
        r.attempt = Some(attempt_for(&owner()));
        q.write(&r).unwrap();
        let e = q.submit(plain_submission("a", "c2")).unwrap_err();
        assert!(e.contains("cancel it first"), "{e}");
        assert!(q.submit(plain_submission("a", "c1")).unwrap().existing);
        assert_eq!(q.cancel("a").unwrap(), Cancelled::Requested);
        assert!(q.get("a").unwrap().cancel_requested);
        assert!(q.cancel_pending("a"));
        // queued / handed-back: removed outright; unknown: error
        q.submit(plain_submission("b", "c1")).unwrap();
        assert_eq!(q.cancel("b").unwrap(), Cancelled::Removed);
        assert!(q.get("b").is_none());
        assert!(q.cancel("zzz").is_err());
        let mut r = q.get("a").unwrap();
        r.phase = Phase::Integrated;
        q.write(&r).unwrap();
        assert!(q.cancel("a").is_err(), "integrated cannot be cancelled");
    }

    #[test]
    fn save_progress_keeps_a_cancel_flag_written_by_someone_else() {
        let tmp = tempfile::tempdir().unwrap();
        let q = Queue::at(tmp.path().join("q"));
        let mut mine = q.submit(plain_submission("a", "c1")).unwrap().request;
        mine.phase = Phase::Integrating;
        q.write(&mine).unwrap();
        // another process flags it while the owner works from its own copy
        assert_eq!(q.cancel("a").unwrap(), Cancelled::Requested);
        assert!(!mine.cancel_requested);
        q.save_progress(&mut mine).unwrap();
        assert!(
            mine.cancel_requested,
            "the owner's write must not erase the flag"
        );
        assert!(q.get("a").unwrap().cancel_requested);
    }

    #[test]
    fn a_second_handle_cannot_take_a_held_turn_until_it_is_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let q = Queue::at(tmp.path().join("q"));
        let held = q.try_turn().unwrap().expect("first turn is free");
        assert!(q.try_turn().unwrap().is_none(), "the turn is exclusive");
        // another Queue value on the same directory (a second project
        // window, or a test's second handle) sees the same lock
        let q2 = Queue::at(tmp.path().join("q"));
        assert!(q2.try_turn().unwrap().is_none());
        drop(held);
        assert!(q2.try_turn().unwrap().is_some());
        // an unrelated repository is independent
        let other = Queue::at(tmp.path().join("other"));
        let _mine = q.try_turn().unwrap().unwrap();
        assert!(other.try_turn().unwrap().is_some());
    }

    /// The child half of the cross-process barrier: with the env var set,
    /// take the turn, signal `held`, wait for `release`. Without it, a
    /// no-op so an ordinary test run is unaffected.
    #[test]
    fn hold_turn_lock_child() {
        let Ok(dir) = std::env::var("FOREMAN_TEST_HOLD_TURN") else {
            return;
        };
        let dir = PathBuf::from(dir);
        let q = Queue::at(dir.clone());
        let turn = q.try_turn().unwrap().expect("child takes the turn");
        std::fs::write(dir.join("held"), "").unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while !dir.join("release").exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        drop(turn);
    }

    #[test]
    fn a_second_process_cannot_take_a_held_turn() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("q");
        let q = Queue::at(dir.clone());
        q.ensure_dir().unwrap();
        let exe = std::env::current_exe().unwrap();
        let mut child = std::process::Command::new(exe)
            .args([
                "--exact",
                "integrate::tests::hold_turn_lock_child",
                "--nocapture",
            ])
            .env("FOREMAN_TEST_HOLD_TURN", &dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn the test binary as the other process");
        let deadline = Instant::now() + Duration::from_secs(30);
        while !dir.join("held").exists() {
            assert!(Instant::now() < deadline, "child never took the turn");
            assert!(
                child.try_wait().unwrap().is_none(),
                "child exited before taking the turn"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            q.try_turn().unwrap().is_none(),
            "a turn held by another process must be refused"
        );
        assert!(
            run_turn(&q, &owner(), &NoHooks).unwrap().is_none(),
            "run_turn reports the held turn and touches nothing"
        );
        std::fs::write(dir.join("release"), "").unwrap();
        let status = child.wait().unwrap();
        assert!(status.success());
        assert!(
            q.try_turn().unwrap().is_some(),
            "the OS released the dead process's turn"
        );
    }

    // -- submission preflight -------------------------------------------------

    #[test]
    fn prepare_submission_needs_a_clean_idle_worktree_on_its_branch() {
        let Some(r) = repo() else { return };
        let wt = bring_up(r.path(), "a1");
        let tree = Path::new(&wt.path);
        let sub = prepare_submission(r.path(), "a1", &wt, None).unwrap();
        assert_eq!(sub.commit, head(tree));
        assert_eq!(sub.target, "main");
        assert_eq!(sub.branch, "card/a1");
        assert_eq!(
            PathBuf::from(&sub.repo),
            repo_identity(r.path()).unwrap(),
            "identity is the common dir, from either checkout"
        );
        assert_eq!(
            repo_identity(tree).unwrap(),
            repo_identity(r.path()).unwrap()
        );
        // dirty
        std::fs::write(tree.join("f.txt"), "edit\n").unwrap();
        let e = prepare_submission(r.path(), "a1", &wt, None).unwrap_err();
        assert!(e.contains("uncommitted changes"), "{e}");
        git_in(tree, &["checkout", "-q", "--", "f.txt"]);
        // wrong branch
        git_in(tree, &["checkout", "-q", "-b", "elsewhere"]);
        let e = prepare_submission(r.path(), "a1", &wt, None).unwrap_err();
        assert!(e.contains("expected card/a1"), "{e}");
        git_in(tree, &["checkout", "-q", "card/a1"]);
        // missing
        let gone = crate::kanban::Worktree {
            path: format!("{}/nope", wt.root().display()),
            ..wt.clone()
        };
        let e = prepare_submission(r.path(), "a1", &gone, None).unwrap_err();
        assert!(e.contains("missing"), "{e}");
    }

    // -- turns ----------------------------------------------------------------

    fn bring_up_branch(repo: &Path, id: &str) -> crate::kanban::Worktree {
        let card =
            crate::kanban::Card::new(id.into(), "t".into(), None, crate::kanban::now_stamp());
        match crate::kanban::bring_up_branch(repo, &card).unwrap() {
            crate::kanban::BringUp::Worktree(wt) if wt.in_place => wt,
            other => panic!("expected a branch record, got {other:?}"),
        }
    }

    #[test]
    fn a_branch_card_integrates_in_place_and_the_humans_changes_survive() {
        let Some(r) = repo() else { return };
        // Uncommitted human work in the shared checkout, before and after
        // the card starts.
        std::fs::write(r.path().join("f.txt"), "human edit\n").unwrap();
        std::fs::write(r.path().join("notes.txt"), "untracked\n").unwrap();
        let wt = bring_up_branch(r.path(), "cc");
        let tip = commit_file(r.path(), "g.txt", "g\n", "card work");
        write_policy(r.path(), &["git", "cat-file", "-e", "HEAD:g.txt"]);
        let (q, sub) = submit(r.path(), "cc", &wt);
        assert!(sub.request.in_place);
        assert_eq!(sub.request.worktree, sub.request.dest);
        let events = turn(&q);
        assert!(
            matches!(&events[..], [TurnEvent::Integrated { card, prepared }] if card == "cc" && *prepared == tip),
            "{events:?}"
        );
        assert_eq!(git_in(r.path(), &["rev-parse", "main"]), tip);
        assert_eq!(
            git_in(r.path(), &["symbolic-ref", "--short", "HEAD"]),
            "main",
            "the checkout is back on the base"
        );
        assert_eq!(
            std::fs::read_to_string(r.path().join("f.txt")).unwrap(),
            "human edit\n"
        );
        assert!(r.path().join("notes.txt").exists());
        let status = git_in(r.path(), &["status", "--porcelain", "--untracked-files=no"]);
        assert_eq!(status, "M f.txt", "only the human's edit is uncommitted");
        let check = q.get("cc").unwrap().attempt.unwrap().check.unwrap();
        assert!(check.ok);
    }

    #[test]
    fn a_branch_card_whose_base_moved_goes_back_to_the_worker_untouched() {
        let Some(r) = repo() else { return };
        let wt = bring_up_branch(r.path(), "cc");
        let tip = commit_file(r.path(), "g.txt", "g\n", "card work");
        // Someone advances main without checking it out.
        let tree = git_in(r.path(), &["rev-parse", "main^{tree}"]);
        let moved = git_in(
            r.path(),
            &["commit-tree", &tree, "-p", "main", "-m", "elsewhere"],
        );
        git_in(r.path(), &["update-ref", "refs/heads/main", &moved]);
        let (q, _) = submit(r.path(), "cc", &wt);
        let events = turn(&q);
        assert!(
            matches!(&events[..], [TurnEvent::NeedsResolution { card, reason: Reason::BaseMoved, .. }] if card == "cc"),
            "{events:?}"
        );
        assert_eq!(git_in(r.path(), &["rev-parse", "main"]), moved);
        assert_eq!(head(r.path()), tip);
        assert_eq!(
            git_in(r.path(), &["symbolic-ref", "--short", "HEAD"]),
            "card/cc"
        );
        let view = &q.views()["cc"];
        assert_eq!(view.phase, Phase::NeedsResolution);
    }

    #[test]
    fn a_failed_check_leaves_the_branch_card_on_its_branch() {
        let Some(r) = repo() else { return };
        let wt = bring_up_branch(r.path(), "cc");
        commit_file(r.path(), "g.txt", "g\n", "card work");
        write_policy(r.path(), &["git", "cat-file", "-e", "HEAD:missing.txt"]);
        let before = git_in(r.path(), &["rev-parse", "main"]);
        let (q, _) = submit(r.path(), "cc", &wt);
        let events = turn(&q);
        assert!(
            matches!(
                &events[..],
                [TurnEvent::NeedsResolution {
                    reason: Reason::CheckFailed,
                    ..
                }]
            ),
            "{events:?}"
        );
        assert_eq!(git_in(r.path(), &["rev-parse", "main"]), before);
        assert_eq!(
            git_in(r.path(), &["symbolic-ref", "--short", "HEAD"]),
            "card/cc"
        );
    }

    #[test]
    fn a_worktree_card_held_by_a_branch_card_does_not_block_it() {
        let Some(r) = repo() else { return };
        let a = bring_up(r.path(), "aa");
        commit_file(Path::new(&a.path), "a.txt", "a\n", "a");
        let c = bring_up_branch(r.path(), "cc");
        let ctip = commit_file(r.path(), "c.txt", "c\n", "c");
        let (q, _) = submit(r.path(), "aa", &a);
        submit(r.path(), "cc", &c);
        // aa is first in line but the checkout is on card/cc: aa holds,
        // cc still integrates and puts the checkout back on main.
        let events = turn(&q);
        assert!(
            matches!(&events[0], TurnEvent::Held { card, .. } if card == "aa"),
            "{events:?}"
        );
        assert!(
            matches!(&events[1], TurnEvent::Integrated { card, .. } if card == "cc"),
            "{events:?}"
        );
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(git_in(r.path(), &["rev-parse", "main"]), ctip);
        q.remove("cc").unwrap();
        // The next turn lands aa on top.
        let events = turn(&q);
        assert!(
            matches!(&events[..], [TurnEvent::Integrated { card, .. }] if card == "aa"),
            "{events:?}"
        );
        assert_eq!(git_in(r.path(), &["rev-parse", "main~1"]), ctip);
    }

    #[test]
    fn a_dead_owners_in_place_merge_is_recovered_and_the_checkout_moved() {
        let Some(r) = repo() else { return };
        let wt = bring_up_branch(r.path(), "cc");
        let tip = commit_file(r.path(), "g.txt", "g\n", "card work");
        let (q, sub) = submit(r.path(), "cc", &wt);
        // Simulate an owner that died right after the ref update.
        git_in(r.path(), &["update-ref", "refs/heads/main", &tip]);
        let mut req = sub.request;
        req.phase = Phase::Integrating;
        req.attempt = Some(attempt_for(&owner()));
        q.write(&req).unwrap();
        let events = turn(&q);
        assert!(
            matches!(&events[..], [TurnEvent::Recovered { card, .. }] if card == "cc"),
            "{events:?}"
        );
        assert_eq!(q.get("cc").unwrap().phase, Phase::Integrated);
        assert_eq!(
            git_in(r.path(), &["symbolic-ref", "--short", "HEAD"]),
            "main"
        );
    }

    #[test]
    fn two_nonconflicting_submissions_land_in_order_and_the_second_is_checked_against_the_first() {
        let Some(r) = repo() else { return };
        let a = bring_up(r.path(), "aa");
        let b = bring_up(r.path(), "bb");
        let ca = commit_file(Path::new(&a.path), "a.txt", "a\n", "a");
        commit_file(Path::new(&b.path), "b.txt", "b\n", "b");
        // the check for b only passes once a.txt exists in b's tree, i.e.
        // once b was rebased onto a's integrated result
        write_policy(r.path(), &["git", "cat-file", "-e", "HEAD:a.txt"]);
        let (q, sa) = submit(r.path(), "aa", &a);
        let (_, sb) = submit(r.path(), "bb", &b);
        assert!(sa.request.seq < sb.request.seq);
        let events = turn(&q);
        assert!(
            matches!(&events[0], TurnEvent::Integrated { card, prepared } if card == "aa" && *prepared == ca),
            "{events:?}"
        );
        assert!(
            matches!(&events[1], TurnEvent::Integrated { card, .. } if card == "bb"),
            "{events:?}"
        );
        let main = head(r.path());
        assert_eq!(
            main,
            head(Path::new(&b.path)),
            "main is b's prepared commit"
        );
        assert_eq!(
            git_in(r.path(), &["rev-parse", "HEAD~1"]),
            ca,
            "b sits on top of a: submission order"
        );
        let rb = q.get("bb").unwrap();
        assert_eq!(rb.phase, Phase::Integrated);
        let check = rb.attempt.unwrap().check.unwrap();
        assert!(check.ok);
        assert_eq!(check.target, ca, "validated against a's integrated result");
        assert_eq!(check.prepared, main);
        assert_eq!(q.views()["aa"].phase, Phase::Integrated);
        // bookkeeping removes; a second turn finds nothing
        q.remove("aa").unwrap();
        q.remove("bb").unwrap();
        assert!(turn(&q).is_empty());
    }

    #[test]
    fn no_configured_checks_is_recorded_as_such() {
        let Some(r) = repo() else { return };
        let a = bring_up(r.path(), "aa");
        commit_file(Path::new(&a.path), "a.txt", "a\n", "a");
        let (q, _) = submit(r.path(), "aa", &a);
        turn(&q);
        let req = q.get("aa").unwrap();
        assert_eq!(req.phase, Phase::Integrated);
        let check = req.attempt.unwrap().check.unwrap();
        assert!(check.command.is_empty() && check.ok);
        assert_eq!(check.tail, "no checks configured");
        assert_eq!(q.views()["aa"].checks.as_deref(), Some("none configured"));
    }

    #[test]
    fn a_conflicting_card_yields_an_independent_card_lands_and_the_resolved_card_reenters() {
        let Some(r) = repo() else { return };
        let a = bring_up(r.path(), "aa");
        let b = bring_up(r.path(), "bb");
        // main moves on f.txt; a also edits f.txt (conflict); b is independent
        commit_file(r.path(), "f.txt", "main version\n", "main moves");
        commit_file(Path::new(&a.path), "f.txt", "a version\n", "a edits f");
        commit_file(Path::new(&b.path), "b.txt", "b\n", "b");
        let (q, _) = submit(r.path(), "aa", &a);
        submit(r.path(), "bb", &b);
        let events = turn(&q);
        assert!(
            matches!(&events[0], TurnEvent::NeedsResolution { card, reason: Reason::Conflict, detail } if card == "aa" && detail.contains("f.txt")),
            "{events:?}"
        );
        assert!(
            matches!(&events[1], TurnEvent::Integrated { card, .. } if card == "bb"),
            "the card behind the conflict still lands: {events:?}"
        );
        let tree_a = Path::new(&a.path);
        assert!(
            git_op_in_progress(tree_a).is_some(),
            "conflict state is preserved in the worker's worktree"
        );
        let va = q.views()["aa"].clone();
        assert_eq!(va.phase, Phase::NeedsResolution);
        assert_eq!(va.reason, Some(Reason::Conflict));
        assert!(va.next.unwrap().contains("rebase --continue"));
        // resubmitting mid-rebase is refused: the worker must finish
        let e = prepare_submission(r.path(), "aa", &a, None).unwrap_err();
        assert!(e.contains("rebase is in progress"), "{e}");
        // the worker resolves and continues
        std::fs::write(tree_a.join("f.txt"), "resolved\n").unwrap();
        git_in(tree_a, &["add", "f.txt"]);
        let out = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "rebase",
                "--continue",
            ])
            .env("GIT_EDITOR", "true")
            .current_dir(tree_a)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(git_op_in_progress(tree_a).is_none());
        let (_, resub) = submit(r.path(), "aa", &a);
        assert!(!resub.existing, "a resolved card is a new tail entry");
        q.remove("bb").unwrap();
        let events = turn(&q);
        assert!(
            matches!(&events[0], TurnEvent::Integrated { card, .. } if card == "aa"),
            "{events:?}"
        );
        // trimmed: autocrlf may check the file out with CRLF
        assert_eq!(
            std::fs::read_to_string(r.path().join("f.txt"))
                .unwrap()
                .trim(),
            "resolved"
        );
        assert!(r.path().join("b.txt").exists(), "a re-entered on top of b");
        assert_eq!(head(r.path()), head(tree_a));
    }

    #[test]
    fn editing_a_queued_branch_is_a_stale_submission_and_the_edit_is_kept() {
        let Some(r) = repo() else { return };
        let a = bring_up(r.path(), "aa");
        let tree = Path::new(&a.path);
        commit_file(tree, "a.txt", "a\n", "a");
        let (q, s) = submit(r.path(), "aa", &a);
        let main_before = head(r.path());
        // the worker keeps going after submitting
        let later = commit_file(tree, "a2.txt", "a2\n", "a2");
        let events = turn(&q);
        assert!(
            matches!(&events[0], TurnEvent::NeedsResolution { reason: Reason::SourceChanged, detail, .. } if detail.contains("moved")),
            "{events:?}"
        );
        assert_eq!(head(r.path()), main_before, "nothing integrated");
        assert_eq!(head(tree), later, "the worker's commit is untouched");
        assert_eq!(q.get("aa").unwrap().commit, s.request.commit);
        // a duplicate submission (same commit) while queued is one request
        let (q2, again) = {
            let mut sub = prepare_submission(r.path(), "aa", &a, None).unwrap();
            sub.commit = s.request.commit.clone();
            let q2 = Queue::for_repo(Path::new(&sub.repo));
            // put it back to queued to model "lost reply, worker retries"
            let mut req = q2.get("aa").unwrap();
            req.phase = Phase::Queued;
            req.outcome = None;
            q2.write(&req).unwrap();
            let again = q2.submit(sub).unwrap();
            (q2, again)
        };
        assert!(again.existing);
        assert_eq!(q2.requests().len(), 1);
    }

    #[test]
    fn a_failed_check_never_advances_the_target_and_reports_the_output() {
        let Some(r) = repo() else { return };
        let a = bring_up(r.path(), "aa");
        let tree = Path::new(&a.path);
        let ca = commit_file(tree, "a.txt", "a\n", "a");
        write_policy(
            r.path(),
            &["git", "rev-parse", "--verify", "refs/heads/does-not-exist"],
        );
        let main_before = head(r.path());
        let (q, _) = submit(r.path(), "aa", &a);
        let events = turn(&q);
        assert!(
            matches!(
                &events[0],
                TurnEvent::NeedsResolution {
                    reason: Reason::CheckFailed,
                    ..
                }
            ),
            "{events:?}"
        );
        assert_eq!(head(r.path()), main_before);
        let req = q.get("aa").unwrap();
        let check = req.attempt.as_ref().unwrap().check.as_ref().unwrap();
        assert!(!check.ok);
        assert_eq!(check.prepared, ca);
        assert_eq!(check.target, main_before);
        assert!(
            req.outcome.as_ref().unwrap().detail.contains("fatal") || !check.tail.is_empty(),
            "git output is captured: {check:?}"
        );
        assert_eq!(q.views()["aa"].checks.as_deref(), Some("failed"));
    }

    /// Advances `main` once, from inside the turn, right before the merge.
    struct MoveTargetOnce {
        root: PathBuf,
        moved: std::cell::Cell<bool>,
        checks: std::cell::Cell<u32>,
    }

    impl TurnHooks for MoveTargetOnce {
        fn after_check(&self, _card: &str) {
            self.checks.set(self.checks.get() + 1);
        }
        fn before_merge(&self, _card: &str) {
            if !self.moved.replace(true) {
                commit_file(&self.root, "moved.txt", "m\n", "external push");
            }
        }
    }

    #[test]
    fn a_moved_target_causes_fresh_preparation_and_validation() {
        let Some(r) = repo() else { return };
        let a = bring_up(r.path(), "aa");
        let tree = Path::new(&a.path);
        commit_file(tree, "a.txt", "a\n", "a");
        let (q, _) = submit(r.path(), "aa", &a);
        let hooks = MoveTargetOnce {
            root: r.path().to_path_buf(),
            moved: false.into(),
            checks: 0.into(),
        };
        let events = run_turn(&q, &owner(), &hooks).unwrap().unwrap();
        assert!(
            matches!(&events[0], TurnEvent::Integrated { card, .. } if card == "aa"),
            "{events:?}"
        );
        assert_eq!(
            hooks.checks.get(),
            2,
            "checked again against the new target"
        );
        assert!(r.path().join("moved.txt").exists());
        assert_eq!(head(r.path()), head(tree));
        assert_eq!(
            git_in(r.path(), &["log", "--format=%s", "-3"]),
            "a\nexternal push\ninit",
            "a was re-prepared on top of the external commit"
        );
        let att = q.get("aa").unwrap().attempt.unwrap();
        assert_eq!(att.retries, 1);
        assert_eq!(att.check.unwrap().target, att.target_commit.unwrap());
    }

    #[test]
    fn a_dirty_or_wrong_destination_holds_the_queue_and_touches_nothing() {
        let Some(r) = repo() else { return };
        let a = bring_up(r.path(), "aa");
        commit_file(Path::new(&a.path), "a.txt", "a\n", "a");
        let (q, _) = submit(r.path(), "aa", &a);
        // dirty destination (tracked edit outside .foreman/)
        std::fs::write(r.path().join("f.txt"), "user wip\n").unwrap();
        // .foreman/ edits alone must not count
        std::fs::create_dir_all(r.path().join(".foreman/tasks")).unwrap();
        std::fs::write(r.path().join(".foreman/tasks/x.json"), "{}").unwrap();
        git_in(r.path(), &["add", ".foreman/tasks/x.json"]);
        git_in(r.path(), &["commit", "-q", "-m", "tasks"]);
        std::fs::write(r.path().join(".foreman/tasks/x.json"), "{\"v\":1}").unwrap();
        let events = turn(&q);
        assert!(
            matches!(&events[0], TurnEvent::Held { detail, .. } if detail.contains("f.txt")),
            "{events:?}"
        );
        assert_eq!(
            std::fs::read_to_string(r.path().join("f.txt")).unwrap(),
            "user wip\n",
            "the user's edit is preserved"
        );
        let v = q.views()["aa"].clone();
        assert_eq!(v.phase, Phase::Queued);
        assert!(v.hold.unwrap().contains("destination dirty"));
        // clean it: only the app-owned .foreman/ edit remains → integrates
        git_in(r.path(), &["checkout", "-q", "--", "f.txt"]);
        let events = turn(&q);
        assert!(
            matches!(&events[0], TurnEvent::Integrated { .. }),
            "{events:?}"
        );
        assert_eq!(
            std::fs::read_to_string(r.path().join(".foreman/tasks/x.json")).unwrap(),
            "{\"v\":1}",
            "the app's card-file edit survives the fast-forward"
        );
        // wrong branch in the destination
        let b = bring_up(r.path(), "bb");
        commit_file(Path::new(&b.path), "b.txt", "b\n", "b");
        submit(r.path(), "bb", &b);
        git_in(r.path(), &["checkout", "-q", "-b", "other"]);
        let events = turn(&q);
        assert!(
            matches!(&events[0], TurnEvent::Held { detail, .. } if detail.contains("expected main")),
            "{events:?}"
        );
        assert!(!r.path().join("b.txt").exists());
    }

    #[test]
    fn a_held_turn_stops_and_leaves_the_cards_behind_it_queued() {
        let Some(r) = repo() else { return };
        let a = bring_up(r.path(), "aa");
        let b = bring_up(r.path(), "bb");
        commit_file(Path::new(&a.path), "a.txt", "a\n", "a");
        commit_file(Path::new(&b.path), "b.txt", "b\n", "b");
        let (q, _) = submit(r.path(), "aa", &a);
        submit(r.path(), "bb", &b);
        std::fs::write(r.path().join("f.txt"), "wip\n").unwrap();
        let events = turn(&q);
        assert_eq!(events.len(), 1, "one hold, then stop: {events:?}");
        let v = q.views();
        assert_eq!(v["aa"].position, Some(1));
        assert_eq!(v["bb"].position, Some(2));
        assert!(v["bb"].hold.is_none(), "only the head carries the hold");
    }

    #[test]
    fn recovery_decides_from_git_state_for_every_crash_point() {
        let Some(r) = repo() else { return };
        // Five cards, each left "integrating" by a dead owner at a
        // different point. Recovery runs under the turn, before any work.
        let before = bring_up(r.path(), "c1"); // crashed before the rebase
        let after = bring_up(r.path(), "c2"); // crashed after the rebase (during checks)
        let merged = bring_up(r.path(), "c3"); // crashed after the merge, before Done
        let midway = bring_up(r.path(), "c4"); // crashed mid-rebase (conflict state)
        let moved = bring_up(r.path(), "c5"); // worker moved the branch meanwhile
        let cb = commit_file(Path::new(&before.path), "c1.txt", "1\n", "c1");
        let ca = commit_file(Path::new(&after.path), "c2.txt", "2\n", "c2");
        let cm = commit_file(Path::new(&merged.path), "c3.txt", "3\n", "c3");
        commit_file(Path::new(&midway.path), "f.txt", "c4\n", "c4");
        let cv = commit_file(Path::new(&moved.path), "c5.txt", "5\n", "c5");
        let (q, _) = submit(r.path(), "c1", &before);
        submit(r.path(), "c2", &after);
        submit(r.path(), "c3", &merged);
        submit(r.path(), "c5", &moved);
        // c3: fast-forward main to its commit by hand (the dead owner did it)
        git_in(r.path(), &["merge", "--ff-only", &cm]);
        // main moves so c4's rebase conflicts
        commit_file(r.path(), "f.txt", "main\n", "main moves");
        submit(r.path(), "c4", &midway);
        let main_now = head(r.path());
        // c2: the dead owner had rebased it onto the moved main (prepared
        // differs from the submitted commit) and died during the checks
        git_in(Path::new(&after.path), &["rebase", &main_now]);
        let ca2 = head(Path::new(&after.path));
        assert_ne!(ca2, ca);
        // c4: leave a real conflicted rebase behind
        let out = std::process::Command::new("git")
            .args(["rebase", &main_now])
            .current_dir(&midway.path)
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(git_op_in_progress(Path::new(&midway.path)).is_some());
        // c5: the worker moved on
        let cv2 = commit_file(Path::new(&moved.path), "c5b.txt", "5b\n", "c5b");
        assert_ne!(cv, cv2);
        // c2: rebased already (prepared == HEAD, no-op rebase onto init)
        let stamp = |card: &str, prepared: Option<&str>, stage: Stage| {
            let mut req = q.get(card).unwrap();
            req.phase = Phase::Integrating;
            let mut att = attempt_for(&Owner {
                pid: 1,
                run: "dead".into(),
            });
            att.stage = stage;
            att.prepared = prepared.map(str::to_string);
            req.attempt = Some(att);
            q.write(&req).unwrap();
        };
        stamp("c1", None, Stage::Preflight);
        stamp("c2", Some(&ca2), Stage::Check);
        stamp("c3", Some(&cm), Stage::Merge);
        stamp("c4", None, Stage::Rebase);
        stamp("c5", Some(&cv), Stage::Check);

        let events = turn(&q);
        let recovered: Vec<(&str, &str)> = events
            .iter()
            .filter_map(|e| match e {
                TurnEvent::Recovered { card, note } => Some((card.as_str(), note.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(recovered.len(), 5, "{events:?}");
        let note = |card: &str| recovered.iter().find(|(c, _)| *c == card).unwrap().1;
        assert!(note("c1").contains("before the rebase"), "{}", note("c1"));
        assert!(note("c2").contains("after the rebase"), "{}", note("c2"));
        assert!(note("c3").contains("already on main"), "{}", note("c3"));
        assert!(
            note("c4").contains("rebase left in progress"),
            "{}",
            note("c4")
        );
        assert!(note("c5").contains("moved"), "{}", note("c5"));
        // c3 is integrated with no second merge; c1 and c2 were queued again
        // and land in this same turn; c4 and c5 are handed back untouched.
        assert_eq!(q.get("c3").unwrap().phase, Phase::Integrated);
        assert_eq!(q.get("c1").unwrap().phase, Phase::Integrated);
        assert_eq!(q.get("c2").unwrap().phase, Phase::Integrated);
        let c4 = q.get("c4").unwrap();
        assert_eq!(c4.phase, Phase::NeedsResolution);
        assert_eq!(c4.outcome.as_ref().unwrap().reason, Reason::Interrupted);
        assert!(
            git_op_in_progress(Path::new(&midway.path)).is_some(),
            "left for the worker"
        );
        let c5 = q.get("c5").unwrap();
        assert_eq!(c5.phase, Phase::NeedsResolution);
        assert_eq!(c5.outcome.as_ref().unwrap().reason, Reason::SourceChanged);
        assert_eq!(head(Path::new(&moved.path)), cv2);
        for f in ["c1.txt", "c2.txt", "c3.txt"] {
            assert!(r.path().join(f).exists(), "{f} landed");
        }
        assert!(!r.path().join("c5.txt").exists());
        let _ = (cb, ca);
    }

    #[test]
    fn cancel_before_the_turn_removes_the_request_and_touches_no_tree() {
        let Some(r) = repo() else { return };
        let a = bring_up(r.path(), "aa");
        let ca = commit_file(Path::new(&a.path), "a.txt", "a\n", "a");
        let (q, _) = submit(r.path(), "aa", &a);
        assert_eq!(q.cancel("aa").unwrap(), Cancelled::Removed);
        assert!(turn(&q).is_empty());
        assert_eq!(head(Path::new(&a.path)), ca);
        assert!(Path::new(&a.path).is_dir());
        assert!(!r.path().join("a.txt").exists());
    }

    /// Flags the request for cancellation from inside the turn, after the
    /// rebase — the worker changing its mind while an owner works.
    struct CancelAfterRebase(Queue);

    impl TurnHooks for CancelAfterRebase {
        fn after_rebase(&self, card: &str) {
            assert_eq!(self.0.cancel(card).unwrap(), Cancelled::Requested);
        }
    }

    #[test]
    fn cancel_during_the_turn_stops_at_the_checkpoint_and_keeps_the_worktree() {
        let Some(r) = repo() else { return };
        let a = bring_up(r.path(), "aa");
        let tree = Path::new(&a.path);
        commit_file(tree, "a.txt", "a\n", "a");
        commit_file(r.path(), "m.txt", "m\n", "main moves");
        let (q, _) = submit(r.path(), "aa", &a);
        let main_before = head(r.path());
        let hooks = CancelAfterRebase(q.clone());
        let events = run_turn(&q, &owner(), &hooks).unwrap().unwrap();
        assert!(
            matches!(&events[0], TurnEvent::Cancelled { note, .. } if note.contains("after the rebase")),
            "{events:?}"
        );
        assert!(q.get("aa").is_none(), "cancelled work leaves the queue");
        assert_eq!(head(r.path()), main_before, "the target never moved");
        assert!(
            tree.join("m.txt").exists(),
            "the rebase result is kept, not undone"
        );
        assert!(git_op_in_progress(tree).is_none());
        assert_eq!(
            git_in(tree, &["symbolic-ref", "--short", "HEAD"]),
            "card/aa",
            "the worker's branch survives"
        );
    }

    /// Cancels from inside the turn after the check, in the window before
    /// the final preflight — and dirties the destination so that preflight
    /// would otherwise hold the request with the flag erased.
    struct CancelAfterCheck {
        queue: Queue,
        root: PathBuf,
    }

    impl TurnHooks for CancelAfterCheck {
        fn after_check(&self, card: &str) {
            assert_eq!(self.queue.cancel(card).unwrap(), Cancelled::Requested);
            std::fs::write(self.root.join("f.txt"), "wip\n").unwrap();
        }
    }

    #[test]
    fn a_cancel_after_the_check_is_honored_and_never_erased_by_a_hold() {
        let Some(r) = repo() else { return };
        let a = bring_up(r.path(), "aa");
        let tree = Path::new(&a.path);
        commit_file(tree, "a.txt", "a\n", "a");
        let (q, _) = submit(r.path(), "aa", &a);
        let main_before = head(r.path());
        let hooks = CancelAfterCheck {
            queue: q.clone(),
            root: r.path().to_path_buf(),
        };
        let events = run_turn(&q, &owner(), &hooks).unwrap().unwrap();
        assert!(
            matches!(&events[0], TurnEvent::Cancelled { note, .. } if note.contains("after the check")),
            "{events:?}"
        );
        assert!(
            q.get("aa").is_none(),
            "a cancelled request leaves the queue"
        );
        assert_eq!(head(r.path()), main_before);
        assert_eq!(
            std::fs::read_to_string(r.path().join("f.txt")).unwrap(),
            "wip\n",
            "the destination's edit is untouched"
        );
        assert!(git_op_in_progress(tree).is_none());
    }

    #[test]
    fn a_resubmission_racing_the_dequeue_is_the_one_that_runs() {
        // Model the race: the turn's unlocked read saw the old request, but
        // by the time it takes the lock a newer submission (new seq, new
        // commit) has replaced it. The fresh file must be what integrates.
        let Some(r) = repo() else { return };
        let a = bring_up(r.path(), "aa");
        let tree = Path::new(&a.path);
        let c1 = commit_file(tree, "a.txt", "a\n", "a");
        let (q, _) = submit(r.path(), "aa", &a);
        let c2 = commit_file(tree, "a2.txt", "a2\n", "a2");
        let (_, resub) = submit(r.path(), "aa", &a);
        assert!(!resub.existing);
        assert_eq!(resub.request.commit, c2);
        assert_ne!(c1, c2);
        let events = turn(&q);
        assert!(
            matches!(&events[0], TurnEvent::Integrated { prepared, .. } if *prepared == c2),
            "{events:?}"
        );
        assert_eq!(q.get("aa").unwrap().commit, c2);
    }

    #[test]
    fn both_pipes_share_one_output_grace() {
        // Senders held, never sent: both pipes are "still open". Waiting the
        // grace per pipe would take at least 2 × grace (recv_timeout never
        // returns early), so anything under that proves one shared deadline.
        let (_out_tx, out_rx) = std::sync::mpsc::channel::<String>();
        let (_err_tx, err_rx) = std::sync::mpsc::channel::<String>();
        let grace = Duration::from_secs(2);
        let start = Instant::now();
        let (out, err) = collect_output(&out_rx, &err_rx, grace);
        let took = start.elapsed();
        assert!(out.is_empty() && err.is_empty());
        assert!(took >= grace, "waited the grace, took {took:?}");
        assert!(
            took < grace * 2,
            "one deadline for both pipes, took {took:?}"
        );
    }

    #[test]
    fn output_already_delivered_is_collected_after_the_deadline() {
        let (out_tx, out_rx) = std::sync::mpsc::channel::<String>();
        let (err_tx, err_rx) = std::sync::mpsc::channel::<String>();
        out_tx.send("out".into()).unwrap();
        err_tx.send("err".into()).unwrap();
        let (out, err) = collect_output(&out_rx, &err_rx, Duration::ZERO);
        assert_eq!((out.as_str(), err.as_str()), ("out", "err"));
    }

    #[cfg(windows)]
    #[test]
    fn a_finished_check_whose_descendant_keeps_the_pipe_open_does_not_hang_the_turn() {
        let Some(r) = repo() else { return };
        let a = bring_up(r.path(), "aa");
        commit_file(Path::new(&a.path), "a.txt", "a\n", "a");
        std::fs::create_dir_all(r.path().join(".foreman")).unwrap();
        // cmd exits at once (success) but leaves a background ping that
        // inherited the captured pipes and runs for ~2 min. The bound below
        // times the whole turn (rebase, check, fast-forward), whose git steps
        // alone ran past 20 s under a parallel `cargo test` with no console
        // (the integration queue's own check) — so it sits far above a loaded
        // turn and far below the descendant's lifetime, failing only if the
        // turn waited on it. The grace's own size is pinned by
        // both_pipes_share_one_output_grace, not here.
        std::fs::write(
            r.path().join(POLICY_FILE),
            r#"{"check":["cmd","/c","start /b ping -n 120 127.0.0.1 & exit 0"],"timeout_secs":600}"#,
        )
        .unwrap();
        let (q, _) = submit(r.path(), "aa", &a);
        let start = Instant::now();
        let events = turn(&q);
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "output collection must be bounded, took {:?}",
            start.elapsed()
        );
        assert!(
            matches!(&events[0], TurnEvent::Integrated { .. }),
            "{events:?}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_check_past_its_timeout_is_killed_and_fails_the_request() {
        let Some(r) = repo() else { return };
        let a = bring_up(r.path(), "aa");
        commit_file(Path::new(&a.path), "a.txt", "a\n", "a");
        std::fs::create_dir_all(r.path().join(".foreman")).unwrap();
        std::fs::write(
            r.path().join(POLICY_FILE),
            r#"{"check":["ping","-n","120","127.0.0.1"],"timeout_secs":1}"#,
        )
        .unwrap();
        let (q, _) = submit(r.path(), "aa", &a);
        let start = Instant::now();
        let events = turn(&q);
        // ~2 min of ping against a 60 s bound: only a missed kill fails it.
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "the check was killed at its deadline"
        );
        assert!(
            matches!(&events[0], TurnEvent::NeedsResolution { reason: Reason::CheckFailed, detail, .. } if detail.contains("timed out")),
            "{events:?}"
        );
        assert!(!r.path().join("a.txt").exists());
    }

    #[test]
    fn a_card_with_no_new_commits_integrates_as_a_no_op() {
        let Some(r) = repo() else { return };
        let a = bring_up(r.path(), "aa");
        let (q, _) = submit(r.path(), "aa", &a);
        let main_before = head(r.path());
        let events = turn(&q);
        assert!(
            matches!(&events[0], TurnEvent::Integrated { prepared, .. } if *prepared == main_before),
            "{events:?}"
        );
        assert_eq!(head(r.path()), main_before);
    }

    #[test]
    fn separate_repositories_integrate_concurrently() {
        let Some(r1) = repo() else { return };
        let Some(r2) = repo() else { return };
        let a = bring_up(r1.path(), "aa");
        let b = bring_up(r2.path(), "bb");
        commit_file(Path::new(&a.path), "a.txt", "a\n", "a");
        commit_file(Path::new(&b.path), "b.txt", "b\n", "b");
        let (q1, _) = submit(r1.path(), "aa", &a);
        let (q2, _) = submit(r2.path(), "bb", &b);
        assert_ne!(q1, q2);
        let _held = q1.try_turn().unwrap().unwrap();
        // r1's turn is taken; r2 integrates regardless
        let events = turn(&q2);
        assert!(
            matches!(&events[0], TurnEvent::Integrated { .. }),
            "{events:?}"
        );
        assert!(run_turn(&q1, &owner(), &NoHooks).unwrap().is_none());
    }
}
