//! Durable sequential assignments and checkout reservations, independent of the GUI.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// CLI/control command. Provider identity is verified by the adapter, never trusted from environment alone.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)] // a CLI and host from different builds share the pipe
pub(crate) struct Command {
    pub operation: String,
    pub id: String,
    pub request_id: String,
    pub provider: String,
    pub session: String,
    pub caller_pid: u32,
    pub generation: Option<u64>,
    pub revision: Option<u64>,
    pub cards: Vec<String>,
    pub plan: Option<String>,
    pub target: String,
    pub note: String,
    pub submit: bool,
    pub recover: bool,
}

impl Command {
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(self.provider.as_str(), "codex" | "claude")
            || self.session.trim().is_empty()
            || self.session.len() > 256
            || self.caller_pid == 0
        {
            return Err(
                "assignment requires a supported --provider and provider --session identity".into(),
            );
        }
        if self.operation == "context" {
            return Ok(());
        }
        if self.operation == "status" && !self.id.is_empty() {
            return Ok(());
        }
        if self.id.is_empty() || self.request_id.is_empty() {
            return Err("assignment requires --id and --request-id".into());
        }
        if self.note.len() > 16 * 1024 {
            return Err("continuation note exceeds 16 KiB".into());
        }
        match self.operation.as_str() {
            "begin" if self.target.is_empty() || (self.cards.is_empty() == self.plan.is_none()) => {
                return Err("begin requires --target and either --cards or --plan".into());
            }
            "begin" => {}
            "continue" | "resume" | "checkpoint" | "pause" | "release" => {
                if self.generation.is_none() || self.revision.is_none() {
                    return Err(
                        "mutation requires --generation and --revision from the last result".into(),
                    );
                }
            }
            _ => return Err("unknown assignment operation".into()),
        }
        if self.submit && self.operation != "continue" {
            return Err("--submit is only valid with continue".into());
        }
        if self.recover && self.operation != "resume" {
            return Err("--recover is only valid with resume".into());
        }
        Ok(())
    }

    /// Execute only after the control adapter validated this owner against the live process tree.
    /// Each result also discloses whether the provider can deliver a compaction brief.
    pub fn execute(&self, cwd: &Path, owner: &OwnerIdentity) -> Result<Snapshot, String> {
        let mut result = self.apply(cwd, owner)?;
        result.recovery = Some(
            crate::agent_hooks::recovery_for(&self.provider)
                .disclosure()
                .into(),
        );
        Ok(result)
    }

    fn apply(&self, cwd: &Path, owner: &OwnerIdentity) -> Result<Snapshot, String> {
        self.validate()?;
        let common = crate::integrate::repo_identity(cwd)?;
        let store = AssignmentStore::for_repo(&common);
        let owner_key = serde_json::to_string(owner).map_err(|e| e.to_string())?;
        if self.operation == "begin" {
            let cards = if let Some(plan) = &self.plan {
                if store.exists(&self.id)? {
                    let existing = store.get(&self.id)?;
                    let intent = existing.begin.ok_or("incomplete begin intent")?;
                    if intent.source_plan.as_ref() != Some(plan) {
                        return Err("begin plan differs from original request".into());
                    }
                    intent.cards
                } else {
                    let mut board = crate::kanban::CardStore::default();
                    board.set_dir(Some(cwd));
                    board
                        .cards()
                        .iter()
                        .filter(|c| {
                            c.state != crate::kanban::CardState::Done
                                && c.planned
                                    .as_ref()
                                    .is_some_and(|p| crate::kanban::same_name(&p.name, plan))
                        })
                        .map(|c| c.id.clone())
                        .collect()
                }
            } else {
                self.cards.clone()
            };
            return store.begin(&Begin {
                id: self.id.clone(),
                request_id: self.request_id.clone(),
                owner: owner_key,
                checkout: cwd.into(),
                target: self.target.clone(),
                cards,
                source_plan: self.plan.clone(),
            });
        }
        let record = store.get(&self.id)?;
        if !crate::kanban::same_path(&record.board.to_string_lossy(), &cwd.to_string_lossy()) {
            return Err("assignment belongs to another project board".into());
        }
        if self.operation == "status" {
            let mut result = record.snapshot();
            let observed = crate::kanban::git(&record.checkout, &["branch", "--show-current"])?;
            let queue = crate::integrate::Queue::for_repo(&common);
            let queued = record.current.as_deref().and_then(|id| queue.get(id));
            result.integration = queued.as_ref().map(|r| r.phase.label().to_string());
            result.next = format!(
                "Read-only status; observed branch {observed}. {}",
                result.next
            );
            if record.owner != owner_key {
                result.outcome = Outcome::RecoveryRequired;
                result.next = format!(
                    "{} Owner differs; inspect unfinished work and explicitly resume --recover with this generation and revision.",
                    result.next
                );
            }
            return Ok(result);
        }
        if self.operation == "resume" {
            let original = record
                .receipts
                .iter()
                .find(|r| r.request.request_id == self.request_id)
                .map(|r| &r.request)
                .or_else(|| {
                    record
                        .pending
                        .as_ref()
                        .filter(|p| p.request.request_id == self.request_id)
                        .map(|p| &p.request)
                });
            if let Some(original) = original {
                if original.action
                    != (Action::Resume {
                        new_owner: owner_key.clone(),
                    })
                    || Some(original.generation) != self.generation
                    || Some(original.revision) != self.revision
                {
                    return Err("resume request id reused with a different payload".into());
                }
                return store.mutate(&self.id, original);
            }
        }
        if self.operation == "resume" && record.owner != owner_key && !self.recover {
            return Err("replacement Session requires explicit resume --recover after inspecting the assignment".into());
        }
        let action = match self.operation.as_str() {
            "continue" => Action::Continue {
                submit: self.submit,
            },
            "checkpoint" => Action::Checkpoint {
                note: self.note.clone(),
            },
            "pause" => Action::Pause,
            "release" => Action::Release,
            "resume" => Action::Resume {
                new_owner: owner_key.clone(),
            },
            _ => return Err("unknown assignment operation".into()),
        };
        store.mutate(
            &self.id,
            &Mutation {
                request_id: self.request_id.clone(),
                owner: if self.operation == "resume" {
                    record.owner
                } else {
                    owner_key
                },
                generation: self.generation.ok_or("generation is required")?,
                revision: self.revision.ok_or("revision is required")?,
                action,
            },
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct OwnerIdentity {
    pub instance: String,
    pub project: String,
    pub terminal: String,
    pub provider: String,
    pub session: String,
    pub process: crate::proc::ProviderProcess,
}

#[cfg(test)]
thread_local! {
    static INTERRUPT_AFTER_SAVE: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Assignment {
    v: u32,
    pub id: String,
    pub checkout: PathBuf,
    board: PathBuf,
    pub owner: String,
    pub generation: u64,
    pub revision: u64,
    pub note: String,
    receipts: Vec<Receipt>,
    begin: Option<Begin>,
    begin_result: Option<Snapshot>,
    approved: Vec<Brief>,
    current: Option<String>,
    transition: Option<Preparation>,
    evidence: Vec<crate::integrate::Request>,
    submission: Option<String>,
    #[serde(default)]
    superseded_submission: Option<String>,
    outcome: Outcome,
    pause_requested: bool,
    pending: Option<Pending>,
    interrupted: Vec<Pending>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Begin {
    pub id: String,
    pub request_id: String,
    pub owner: String,
    pub checkout: PathBuf,
    pub target: String,
    pub cards: Vec<String>,
    pub source_plan: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Brief {
    id: String,
    title: String,
    body: Option<String>,
    planned: Option<crate::kanban::Planned>,
}

impl From<&crate::kanban::Card> for Brief {
    fn from(card: &crate::kanban::Card) -> Self {
        Self {
            id: card.id.clone(),
            title: card.title.clone(),
            body: card.body.clone(),
            planned: card.planned.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Preparation {
    card: String,
    base_commit: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Outcome {
    Work,
    Waiting,
    Blocked,
    ScopeChanged,
    Paused,
    Complete,
    Released,
    RecoveryRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Mutation {
    pub request_id: String,
    pub owner: String,
    pub generation: u64,
    pub revision: u64,
    pub action: Action,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub(crate) enum Action {
    Checkpoint { note: String },
    Continue { submit: bool },
    Pause,
    Release,
    Resume { new_owner: String },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Receipt {
    request: Mutation,
    result: Snapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Pending {
    request: Mutation,
    starting_card: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Snapshot {
    pub id: String,
    pub generation: u64,
    pub revision: u64,
    pub note: String,
    pub outcome: Outcome,
    pub current: Option<String>,
    pub branch: Option<String>,
    pub brief: Option<String>,
    pub scope: Vec<String>,
    pub next: String,
    pub integration: Option<String>,
    /// Whether the provider can deliver a compaction brief; reported, not persisted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<String>,
}

impl Assignment {
    /// Complete and Released end the assignment; no further mutation applies.
    fn finished(&self) -> bool {
        matches!(self.outcome, Outcome::Complete | Outcome::Released)
    }

    fn keep_receipt(&mut self, request: &Mutation, result: &Snapshot) {
        self.receipts.push(Receipt {
            request: request.clone(),
            result: result.clone(),
        });
        let excess = self.receipts.len().saturating_sub(RECEIPT_LIMIT);
        self.receipts.drain(..excess);
    }

    /// An unfinished transition keeps the checkout reserved even after the end.
    fn reserves_checkout(&self) -> bool {
        !self.finished() || self.pending.is_some()
    }

    fn verify_accepted_commits(&self) -> Result<(), String> {
        if self.evidence.is_empty() {
            return Ok(());
        }
        let target = &self
            .begin
            .as_ref()
            .ok_or("missing assignment intent")?
            .target;
        for accepted in &self.evidence {
            if accepted.phase != crate::integrate::Phase::Integrated
                || accepted.target != *target
                || crate::kanban::git(
                    &self.checkout,
                    &[
                        "merge-base",
                        "--is-ancestor",
                        &accepted.commit,
                        &format!("refs/heads/{target}"),
                    ],
                )
                .is_err()
            {
                return Err(format!(
                    "accepted card {} is no longer verified on {target}; reconcile target movement before continuing",
                    accepted.card
                ));
            }
        }
        Ok(())
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot { id: self.id.clone(), generation: self.generation, revision: self.revision, note: self.note.clone(), integration: None, recovery: None,
            outcome: self.outcome.clone(), current: self.current.clone(),
            branch: self.current.as_ref().map(|id| format!("card/{id}")),
            brief: self.current.as_ref().and_then(|id| self.approved.iter().find(|b| b.id == *id))
                .map(|b| format!("{}\n{}", b.title, b.body.as_deref().unwrap_or(""))),
            scope: self.approved.iter().map(|b| format!("{}: {}", b.id, b.title)).collect(),
            next: match self.outcome {
                Outcome::Work => "implement and validate the current brief on the prepared branch; checkpoint consequential decisions; commit only when authorized, then continue --submit",
                Outcome::Waiting => "wait for integration or earlier-wave work, then continue with a new request id; do not start another card",
                Outcome::Blocked => "inspect the blocked or missing approved card; do not reclaim it automatically",
                Outcome::ScopeChanged => "review the changed brief with the user; the approved scope has not changed",
                Outcome::Paused => "checkout remains reserved; resume when ready",
                Outcome::Complete => "approved assignment is complete; other plan work may remain",
                Outcome::Released => "checkout reservation released; Git and the board are unchanged, so settle the current card's branch and claim explicitly",
                Outcome::RecoveryRequired => "inspect Git and integration evidence; resolve obvious conflicts and rerun affected checks; ask the user only about ambiguous or destructive choices",
            }.into() }
    }
}

/// Git's discovery of an ordinary checkout from files alone, as (canonical
/// common directory, checkout root). Legacy card guards run on the GUI thread
/// and must not spawn git. `GIT_DIR` and bare layouts are not modeled; those
/// and non-repositories return None, which the guards treat as unreserved.
fn discover_checkout(dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let start = dir.canonicalize().ok()?;
    let root = start.ancestors().find(|p| p.join(".git").exists())?;
    let dotgit = root.join(".git");
    let gitdir = if dotgit.is_dir() {
        dotgit
    } else {
        let text = std::fs::read_to_string(&dotgit).ok()?;
        root.join(text.trim().strip_prefix("gitdir:")?.trim())
    };
    let common = match std::fs::read_to_string(gitdir.join("commondir")) {
        Ok(text) => gitdir.join(text.trim()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => gitdir,
        Err(_) => return None,
    };
    let unverbatim = |p: PathBuf| {
        let s = p.to_string_lossy();
        s.strip_prefix(r"\\?\")
            .map(PathBuf::from)
            .unwrap_or_else(|| p.clone())
    };
    Some((
        unverbatim(common.canonicalize().ok()?),
        unverbatim(root.to_path_buf()),
    ))
}

/// Replays only need recent receipts: an older request ID carries a stale
/// revision and is refused rather than applied again.
const RECEIPT_LIMIT: usize = 16;

/// Prefix of the error a contended assignment lock returns; callers may retry it.
pub(crate) const BUSY: &str = "assignments busy; retry";

pub(crate) struct AssignmentStore {
    dir: PathBuf,
}

/// Keeps the checkout transaction locked from before provider launch through claim persistence.
pub(crate) struct DispatchGuard {
    _lock: Option<File>,
    board: PathBuf,
    card: String,
}

impl DispatchGuard {
    pub fn acquire(
        board: &Path,
        card: &str,
        worktree: Option<&crate::kanban::Worktree>,
    ) -> Result<Self, String> {
        let lock = if worktree.is_some_and(|w| !w.in_place) {
            guard_card(board, card)?
        } else {
            guard_checkout(board)?
        };
        Ok(Self {
            _lock: lock,
            board: board.into(),
            card: card.into(),
        })
    }

    pub fn check(&self, board: &Path, card: &str) -> Result<(), String> {
        if self.card != card
            || !crate::kanban::same_path(&self.board.to_string_lossy(), &board.to_string_lossy())
        {
            return Err("dispatch reservation does not match this card and board".into());
        }
        Ok(())
    }
}

/// Hold through a cooperating legacy operation. A reservation never expires with a process lock.
pub(crate) fn guard_checkout(checkout: &Path) -> Result<Option<File>, String> {
    let Some((common, root)) = discover_checkout(checkout) else {
        return Ok(None);
    };
    let store = AssignmentStore::for_repo(&common);
    let lock = store.lock()?;
    let root = root.to_string_lossy();
    for record in store.records()? {
        if record.reserves_checkout()
            && crate::kanban::same_path(&root, &record.checkout.to_string_lossy())
        {
            return Err(format!(
                "checkout reserved by assignment {}; use assignment continue or resume",
                record.id
            ));
        }
    }
    store.register_board(checkout)?;
    Ok(Some(lock))
}

/// Preserve acceptance evidence before the integration coordinator removes a
/// finished queue entry. A busy assignment keeps the entry available for retry.
pub(crate) fn archive_integration(request: &crate::integrate::Request) -> Result<(), String> {
    if request.phase != crate::integrate::Phase::Integrated {
        return Ok(());
    }
    let store = AssignmentStore::for_repo(Path::new(&request.repo));
    if !store.dir.exists() {
        return Ok(());
    }
    let _lock = store.lock()?;
    for mut record in store.records()? {
        if record.current.as_deref() != Some(request.card.as_str())
            || record.submission.as_deref() != Some(request.commit.as_str())
        {
            continue;
        }
        if !record.evidence.iter().any(|r| r.card == request.card) {
            record.evidence.push(request.clone());
            store.save(&record)?;
        }
    }
    Ok(())
}

/// Guard legacy card release/closeout independently of the project's checkout.
pub(crate) fn guard_card(root: &Path, id: &str) -> Result<Option<File>, String> {
    let Some((common, _)) = discover_checkout(root) else {
        return Ok(None);
    };
    let store = AssignmentStore::for_repo(&common);
    // No assignment has ever begun here: nothing to guard, nothing to create.
    if !store.dir.exists() {
        return Ok(None);
    }
    let lock = store.lock()?;
    for record in store.records()? {
        if record.reserves_checkout() && record.approved.iter().any(|b| b.id == id) {
            return Err(format!(
                "card {id} belongs to assignment {}; reconcile the assignment first",
                record.id
            ));
        }
    }
    Ok(Some(lock))
}

/// Authorize an integration step against checkout ownership. Hold the lock
/// only across a short step; a turn's checks run without it.
pub(crate) fn guard_integration(request: &crate::integrate::Request) -> Result<File, String> {
    let store = AssignmentStore::for_repo(Path::new(&request.repo));
    let lock = store.lock()?;
    for record in store.records()? {
        if !record.reserves_checkout() {
            continue;
        }
        let checkout = record.checkout.to_string_lossy();
        if !crate::kanban::same_path(&checkout, &request.dest)
            && !crate::kanban::same_path(&checkout, &request.worktree)
        {
            continue;
        }
        let authorized = request.in_place
            && record.current.as_deref() == Some(&request.card)
            && record.submission.as_deref() == Some(&request.commit)
            && record
                .begin
                .as_ref()
                .is_some_and(|b| b.target == request.target)
            && crate::kanban::same_path(&checkout, &request.worktree);
        if !authorized {
            return Err(format!("checkout reserved by assignment {}", record.id));
        }
    }
    Ok(lock)
}

impl AssignmentStore {
    pub fn for_repo(common_dir: &Path) -> Self {
        Self {
            dir: common_dir.join("foreman/assignments"),
        }
    }

    fn register_board(&self, board: &Path) -> Result<(), String> {
        use sha2::Digest;
        let board = board.canonicalize().map_err(|e| e.to_string())?;
        let key = format!(
            "{:x}",
            sha2::Sha256::digest(board.to_string_lossy().as_bytes())
        );
        let registry = self.dir.join("boards");
        std::fs::create_dir_all(&registry).map_err(|e| e.to_string())?;
        let path = registry.join(format!("{key}.json"));
        if path.exists() {
            return Ok(());
        }
        let _metadata = self.lock_named("meta.lock")?;
        let tmp = path.with_extension("tmp");
        let mut file = File::create(&tmp).map_err(|e| e.to_string())?;
        file.write_all(&serde_json::to_vec(&board).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        std::fs::rename(tmp, path).map_err(|e| e.to_string())
    }

    fn refuse_legacy_writer(&self, board: &Path, checkout: &Path) -> Result<(), String> {
        self.register_board(board)?;
        for entry in std::fs::read_dir(self.dir.join("boards")).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let board: PathBuf =
                serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            let tasks = board.join(".foreman/tasks");
            if !tasks.exists() {
                continue;
            }
            let root = crate::kanban::git(&board, &["rev-parse", "--show-toplevel"])?;
            if !crate::kanban::same_path(&root, &checkout.to_string_lossy()) {
                continue;
            }
            for entry in std::fs::read_dir(tasks).map_err(|e| e.to_string())? {
                let path = entry.map_err(|e| e.to_string())?.path();
                if path.extension().is_none_or(|e| e != "json") {
                    continue;
                }
                let card: crate::kanban::Card =
                    serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
                        .map_err(|e| e.to_string())?;
                if card.state == crate::kanban::CardState::InProgress
                    && card.worktree.as_ref().is_none_or(|w| w.in_place)
                {
                    return Err(format!(
                        "checkout still has legacy card {} in progress on board {}; resolve its ownership first",
                        card.id,
                        board.display()
                    ));
                }
            }
        }
        Ok(())
    }

    /// Read-only recovery for an already validated provider Session. Notes never
    /// replace observed Git/queue evidence and this path grants no execution authority.
    pub fn recovery_brief(&self, owner: &str) -> Result<Option<String>, String> {
        if !self.dir.exists() {
            return Ok(None);
        }
        let records: Vec<_> = self
            .records()?
            .into_iter()
            .filter(|r| r.owner == owner && !r.finished())
            .collect();
        if records.len() > 1 {
            return Err(
                "ambiguous assignments for this Session; inspect and resume explicitly".into(),
            );
        }
        let Some(record) = records.first() else {
            return Ok(None);
        };
        let snapshot = record.snapshot();
        let observed =
            crate::integrate::recovery_probe(&record.checkout, &["branch", "--show-current"])?;
        let head = crate::integrate::recovery_probe(&record.checkout, &["rev-parse", "HEAD"])?;
        let common = self
            .dir
            .parent()
            .and_then(Path::parent)
            .ok_or("invalid assignment store")?;
        let queue = crate::integrate::Queue::for_repo(common);
        let integration = record
            .current
            .as_deref()
            .and_then(|id| queue.get(id))
            .map(|r| format!("{} at {}", r.phase.label(), r.commit))
            .unwrap_or_else(|| "no live queue entry; reconcile durable evidence".into());
        let expected = snapshot.branch.as_deref().unwrap_or_else(|| {
            record
                .begin
                .as_ref()
                .map_or("unknown", |b| b.target.as_str())
        });
        let disagreement = if observed != expected {
            "Git disagreement: expected and observed branches differ; do not follow stale notes."
        } else {
            "Git branch agrees with the assignment."
        };
        let bounded = |s: &str, limit| s.chars().take(limit).collect::<String>();
        Ok(Some(format!(
            "Foreman assignment {} (generation {}, revision {}).\nApproved scope:\n{}\nCurrent card: {:?}; expected branch: {expected}; observed branch: {observed}; HEAD: {head}.\n{disagreement}\nIntegration: {integration}.\nCurrent brief:\n{}\nContinuation note (reasoning, not authority):\n{}\nRead-only recovery: run assignment continue or resume to reconcile before executing. {}",
            record.id,
            record.generation,
            record.revision,
            bounded(&snapshot.scope.join("\n"), 2000),
            record.current,
            bounded(snapshot.brief.as_deref().unwrap_or("none"), 4000),
            bounded(&record.note, 4000),
            snapshot.next
        )))
    }

    fn path(&self, id: &str) -> Result<PathBuf, String> {
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err("assignment id must contain only letters, digits and hyphens".into());
        }
        Ok(self.dir.join(format!("{id}.json")))
    }

    /// Where a finished record moves, out of the set every guard parses.
    fn finished_path(&self, id: &str) -> Result<PathBuf, String> {
        self.path(id)?;
        Ok(self.dir.join("finished").join(format!("{id}.json")))
    }

    /// An ID names one assignment forever, active or finished.
    fn exists(&self, id: &str) -> Result<bool, String> {
        Ok(self.path(id)?.exists() || self.finished_path(id)?.exists())
    }

    fn lock(&self) -> Result<File, String> {
        self.lock_named("operation.lock")
    }

    fn lock_named(&self, name: &str) -> Result<File, String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.dir.join(name))
            .map_err(|e| e.to_string())?;
        file.try_lock().map_err(|e| format!("{BUSY}: {e}"))?;
        Ok(file)
    }

    /// Read a complete durable record. Corruption fails closed, never frees a checkout.
    pub fn get(&self, id: &str) -> Result<Assignment, String> {
        let data = match std::fs::read(self.path(id)?) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::read(self.finished_path(id)?)
            }
            data => data,
        }
        .map_err(|e| e.to_string())?;
        let record: Assignment = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
        if record.v != 1 || record.id != id {
            return Err("unsupported or mismatched assignment record".into());
        }
        Ok(record)
    }

    fn records(&self) -> Result<Vec<Assignment>, String> {
        let mut records = Vec::new();
        for entry in std::fs::read_dir(&self.dir).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                let id = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .ok_or("invalid assignment filename")?;
                records.push(self.get(id)?);
            }
        }
        Ok(records)
    }

    fn save(&self, record: &Assignment) -> Result<(), String> {
        let _metadata = self.lock_named("meta.lock")?;
        let active = self.path(&record.id)?;
        // A finished record leaves the active set: guards stop parsing it,
        // and damage to it can no longer block the repository. It is written
        // before the active copy goes, and `get` prefers the active copy.
        let archive = !record.reserves_checkout();
        let path = if archive {
            let path = self.finished_path(&record.id)?;
            std::fs::create_dir_all(self.dir.join("finished")).map_err(|e| e.to_string())?;
            path
        } else {
            active.clone()
        };
        let tmp = path.with_extension("tmp");
        let mut file = File::create(&tmp).map_err(|e| e.to_string())?;
        file.write_all(&serde_json::to_vec_pretty(record).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        std::fs::rename(tmp, path).map_err(|e| e.to_string())?;
        if archive {
            match std::fs::remove_file(&active) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.to_string()),
            }
        }
        #[cfg(test)]
        if INTERRUPT_AFTER_SAVE.with(|remaining| match remaining.get() {
            Some(1) => {
                remaining.set(None);
                true
            }
            Some(n) => {
                remaining.set(Some(n - 1));
                false
            }
            None => false,
        }) {
            return Err("simulated interruption after durable write".into());
        }
        Ok(())
    }

    /// Reserve a canonical checkout. OS lock release does not release this reservation.
    pub fn reserve(&self, id: &str, checkout: &Path, owner: &str) -> Result<Assignment, String> {
        let _lock = self.lock()?;
        let record = self.reservation(id, checkout, owner)?;
        self.save(&record)?;
        Ok(record)
    }

    fn reservation(&self, id: &str, checkout: &Path, owner: &str) -> Result<Assignment, String> {
        if self.exists(id)? {
            return Err(format!("assignment id {id} is already used"));
        }
        if owner.trim().is_empty() {
            return Err("owner is required".into());
        }
        let checkout = checkout.canonicalize().map_err(|e| e.to_string())?;
        for record in self.records()? {
            if record.id == id
                || (record.reserves_checkout()
                    && crate::kanban::same_path(
                        &record.checkout.to_string_lossy(),
                        &checkout.to_string_lossy(),
                    ))
            {
                return Err(format!("checkout reserved by assignment {}", record.id));
            }
        }
        let record = Assignment {
            v: 1,
            id: id.into(),
            board: checkout.clone(),
            checkout,
            owner: owner.into(),
            generation: 1,
            revision: 1,
            note: String::new(),
            receipts: Vec::new(),
            begin: None,
            begin_result: None,
            approved: Vec::new(),
            current: None,
            transition: None,
            evidence: Vec::new(),
            submission: None,
            superseded_submission: None,
            outcome: Outcome::Waiting,
            pause_requested: false,
            pending: None,
            interrupted: Vec::new(),
        };
        Ok(record)
    }

    /// Capture approved briefs durably, then prepare the first branch and claim.
    /// Retrying the identical request recovers a partially prepared branch.
    pub fn begin(&self, request: &Begin) -> Result<Snapshot, String> {
        use crate::kanban::{CardState, CardStore, git};
        if request.request_id.is_empty() || request.cards.is_empty() {
            return Err("begin requires a request id and approved cards".into());
        }
        let _lock = self.lock()?;
        let mut board = CardStore::default();
        board.set_dir(Some(&request.checkout));
        if !self.exists(&request.id)? {
            let branch = git(&request.checkout, &["branch", "--show-current"])?;
            if branch != request.target {
                return Err("checkout is not on the requested integration target".into());
            }
            let mut approved = Vec::new();
            for id in &request.cards {
                let card = board
                    .get(id)
                    .ok_or_else(|| format!("missing approved card {id}"))?;
                if card.state != CardState::Backlog {
                    return Err(format!("card {id} is not in backlog"));
                }
                if approved.iter().any(|c: &Brief| c.id == *id) {
                    return Err("duplicate approved card".into());
                }
                approved.push(Brief::from(card));
            }
            let root = git(&request.checkout, &["rev-parse", "--show-toplevel"])?;
            self.refuse_legacy_writer(&request.checkout, Path::new(&root))?;
            let queue = crate::integrate::Queue::for_repo(&crate::integrate::repo_identity(
                &request.checkout,
            )?);
            if queue.requests().iter().any(|r| {
                r.phase != crate::integrate::Phase::Integrated
                    && (crate::kanban::same_path(&r.dest, &root)
                        || crate::kanban::same_path(&r.worktree, &root))
            }) {
                return Err("checkout has pending legacy integration; reconcile it before beginning an assignment".into());
            }
            let mut record = self.reservation(&request.id, Path::new(&root), &request.owner)?;
            record.board = request.checkout.canonicalize().map_err(|e| e.to_string())?;
            record.begin = Some(request.clone());
            record.approved = approved;
            self.save(&record)?;
        }
        let mut record = self.get(&request.id)?;
        if record.owner != request.owner || record.generation != 1 {
            return Err("begin belongs to a previous owner generation; resume explicitly".into());
        }
        if record.begin.as_ref() != Some(request) {
            return Err("assignment begin request differs from its durable intent".into());
        }
        if let Some(result) = &record.begin_result {
            return Ok(result.clone());
        }
        if record.current.is_none() {
            self.prepare_next(&mut record, &mut board)?;
        }
        let result = record.snapshot();
        record.begin_result = Some(result.clone());
        self.save(&record)?;
        Ok(result)
    }

    fn prepare_next(
        &self,
        record: &mut Assignment,
        board: &mut crate::kanban::CardStore,
    ) -> Result<(), String> {
        use crate::kanban::{CardState, git};
        record.verify_accepted_commits()?;
        board.reload();
        let target = record
            .begin
            .as_ref()
            .ok_or("missing begin intent")?
            .target
            .clone();
        if record.transition.is_none()
            && git(&record.checkout, &["branch", "--show-current"])? != target
        {
            return Err("checkout branch moved; reconcile before advancing or completing".into());
        }
        if record.transition.is_none() && record.pause_requested {
            record.outcome = Outcome::Paused;
            return self.save(record);
        }
        if record.transition.is_none()
            && record
                .approved
                .iter()
                .all(|b| record.evidence.iter().any(|r| r.card == b.id))
        {
            record.outcome = Outcome::Complete;
            return self.save(record);
        }
        let card = if let Some(intent) = &record.transition {
            board
                .get(&intent.card)
                .cloned()
                .ok_or("pending card is missing")?
        } else {
            let card = board
                .cards()
                .iter()
                .filter(|c| record.approved.iter().any(|b| b.id == c.id))
                .filter(|c| c.state == CardState::Backlog)
                .filter(|c| {
                    c.planned.as_ref().is_none_or(|plan| {
                        !board.cards().iter().any(|other| {
                            other.state != CardState::Done
                                && other.planned.as_ref().is_some_and(|p| {
                                    crate::kanban::same_name(&p.name, &plan.name)
                                        && p.wave < plan.wave
                                })
                        })
                    })
                })
                .min_by(|a, b| a.created.cmp(&b.created).then(a.id.cmp(&b.id)))
                .cloned();
            let Some(card) = card else {
                record.outcome = if record.approved.iter().any(|b| {
                    board
                        .get(&b.id)
                        .is_none_or(|c| c.state == CardState::Blocked)
                }) {
                    Outcome::Blocked
                } else {
                    Outcome::Waiting
                };
                return self.save(record);
            };
            if record.approved.iter().find(|b| b.id == card.id) != Some(&Brief::from(&card)) {
                record.outcome = Outcome::ScopeChanged;
                return self.save(record);
            }
            if git(&record.checkout, &["branch", "--show-current"])? != target {
                return Err("checkout branch moved; reconcile before claiming".into());
            }
            let branch = format!("card/{}", card.id);
            if crate::kanban::branch_exists(&record.checkout, &branch)? {
                return Err("card branch already exists; reconcile before claiming".into());
            }
            record.transition = Some(Preparation {
                card: card.id.clone(),
                base_commit: git(&record.checkout, &["rev-parse", "HEAD"])?,
            });
            self.save(record)?;
            card
        };
        let intent = record
            .transition
            .as_ref()
            .ok_or("missing preparation intent")?;
        if record.approved.iter().find(|b| b.id == card.id) != Some(&Brief::from(&card)) {
            record.outcome = Outcome::ScopeChanged;
            return self.save(record);
        }
        let branch = format!("card/{}", card.id);
        let current = git(&record.checkout, &["branch", "--show-current"])?;
        if git(&record.checkout, &["rev-parse", "HEAD"])? != intent.base_commit {
            return Err("checkout commit changed during preparation; reconcile".into());
        }
        if current == target {
            if crate::kanban::branch_exists(&record.checkout, &branch)? {
                return Err("unexpected card branch; reconcile before switching".into());
            }
            git(&record.checkout, &["switch", "--no-track", "-c", &branch])?;
        } else if current != branch {
            return Err("unexpected checkout branch during preparation".into());
        }
        let upstream = git(
            &record.checkout,
            &[
                "for-each-ref",
                "--format=%(upstream)",
                &format!("refs/heads/{branch}"),
            ],
        )?;
        if !upstream.is_empty() && upstream != format!("refs/remotes/origin/{branch}") {
            return Err("prepared branch has an unsafe upstream".into());
        }
        if card.state == CardState::Backlog {
            let (terminal, run) = claim_identity(&record.owner);
            board.claim_assignment(
                &card.id,
                &terminal,
                &run,
                crate::kanban::branch_layout(&record.checkout, &card.id, &target),
            )?;
        } else if card.state != CardState::InProgress
            || card
                .claim
                .as_ref()
                .is_none_or(|c| c.terminal != claim_identity(&record.owner).0)
        {
            return Err("card claim changed during preparation".into());
        }
        record.current = Some(card.id);
        record.outcome = Outcome::Work;
        record.transition = None;
        self.save(record)
    }

    fn advance(&self, record: &mut Assignment, submit: bool) -> Result<(), String> {
        use crate::kanban::{CardStore, git};
        let mut board = CardStore::default();
        board.set_dir(Some(&record.board));
        let queue =
            crate::integrate::Queue::for_repo(&crate::integrate::repo_identity(&record.checkout)?);
        if let Some(id) = record.current.clone() {
            let card = board.get(&id).ok_or("current card is missing")?;
            if let Some(request) = queue
                .get(&id)
                .or_else(|| record.evidence.iter().find(|r| r.card == id).cloned())
            {
                let retrying_resubmission = submit
                    && request.phase == crate::integrate::Phase::NeedsResolution
                    && record.superseded_submission.as_ref() == Some(&request.commit)
                    && record
                        .pending
                        .as_ref()
                        .is_some_and(|p| p.request.action == Action::Continue { submit: true });
                if record.submission.as_ref() != Some(&request.commit) && !retrying_resubmission {
                    return Err("integration request differs from assignment submission".into());
                }
                match request.phase {
                    crate::integrate::Phase::Queued | crate::integrate::Phase::Integrating => {
                        record.outcome = Outcome::Waiting;
                        return Ok(());
                    }
                    crate::integrate::Phase::NeedsResolution => {
                        record.outcome = Outcome::RecoveryRequired;
                        if !submit {
                            return Ok(());
                        }
                    }
                    crate::integrate::Phase::Integrated => {
                        let target = record
                            .begin
                            .as_ref()
                            .ok_or("missing assignment intent")?
                            .target
                            .clone();
                        if request.target != target
                            || !request.in_place
                            || !crate::kanban::same_path(
                                &request.worktree,
                                &record.checkout.to_string_lossy(),
                            )
                        {
                            return Err("integration destination differs from assignment".into());
                        }
                        git(
                            &record.checkout,
                            &[
                                "merge-base",
                                "--is-ancestor",
                                &request.commit,
                                &format!("refs/heads/{target}"),
                            ],
                        )?;
                        if !record.evidence.iter().any(|r| r.card == id) {
                            record.evidence.push(request);
                            self.save(record)?;
                        }
                        board.integrated(&id)?;
                        record.current = None;
                        record.submission = None;
                        self.save(record)?;
                        return self.prepare_next(record, &mut board);
                    }
                }
            }
            if card.state == crate::kanban::CardState::Blocked {
                record.outcome = Outcome::Blocked;
                return Ok(());
            }
            let (terminal, run) = claim_identity(&record.owner);
            if card.state != crate::kanban::CardState::InProgress
                || card
                    .claim
                    .as_ref()
                    .is_none_or(|claim| claim.terminal != terminal || claim.run != run)
            {
                return Err("current assignment claim changed; resume and reconcile".into());
            }
            let wt = card
                .worktree
                .as_ref()
                .ok_or("current card has no prepared branch")?;
            let submission = crate::integrate::prepare_submission(
                &record.checkout,
                &id,
                wt,
                Some(record.owner.clone()),
            )?;
            if !submit {
                // Only --submit queues work. A recorded submission with no
                // live request was cancelled or removed; resubmitting is the
                // agent's decision, not a side effect of observing.
                record.outcome = if record.submission.is_some() {
                    Outcome::RecoveryRequired
                } else {
                    Outcome::Work
                };
                return Ok(());
            }
            if record.submission.as_ref() != Some(&submission.commit) {
                record.superseded_submission = record.submission.replace(submission.commit.clone());
            }
            self.save(record)?;
            queue.submit_assignment(submission)?;
            record.superseded_submission = None;
            record.outcome = Outcome::Waiting;
            return Ok(());
        }
        self.prepare_next(record, &mut board)
    }

    /// Compare-and-swap mutation with durable replay after a lost response.
    /// Request IDs cannot be reused for a different payload or owner generation.
    pub fn mutate(&self, id: &str, request: &Mutation) -> Result<Snapshot, String> {
        if request.request_id.trim().is_empty() {
            return Err("request id is required".into());
        }
        let _lock = self.lock()?;
        let mut record = self.get(id)?;
        if record.finished()
            && record.pending.is_none()
            && !record
                .receipts
                .iter()
                .any(|r| r.request.request_id == request.request_id)
        {
            return Err("assignment is finished and its checkout reservation is released; begin a new assignment for new work".into());
        }
        if let Action::Resume { new_owner } = &request.action {
            return self.resume_mutation(record, request, new_owner);
        }
        if request.owner != record.owner || request.generation != record.generation {
            return Err("stale assignment owner; resume and reconcile".into());
        }
        if let Some(receipt) = record
            .receipts
            .iter()
            .find(|r| r.request.request_id == request.request_id)
        {
            return if receipt.request == *request {
                Ok(receipt.result.clone())
            } else {
                Err("request id reused with a different payload".into())
            };
        }
        if request.revision != record.revision {
            return Err("assignment revision changed; read and reconcile".into());
        }
        if record
            .pending
            .as_ref()
            .is_none_or(|pending| pending.request != *request)
        {
            if let Some(pending) = record.pending.take() {
                // Steps are re-entrant from durable state, so a new request
                // may reconcile whatever a failed or lost one left behind.
                // Not after an owner transfer, and not once it claimed a
                // successor: only its replay can report that new card.
                let claimed_successor =
                    pending.starting_card != record.current && record.current.is_some();
                if matches!(pending.request.action, Action::Resume { .. }) || claimed_successor {
                    return Err(
                        "an interrupted request must be reconciled before a new mutation".into(),
                    );
                }
                // Dropped, not kept: only interrupted Resumes are ever consulted.
            }
            record.pending = Some(Pending {
                request: request.clone(),
                starting_card: record.current.clone(),
            });
            self.save(&record)?;
        }
        match &request.action {
            Action::Checkpoint { note } => record.note = note.clone(),
            Action::Continue { submit } => {
                let already_advanced = record
                    .pending
                    .as_ref()
                    .is_some_and(|p| p.starting_card != record.current && record.current.is_some());
                if !already_advanced {
                    self.advance(&mut record, *submit)?;
                }
            }
            Action::Pause => record.pause_requested = true,
            Action::Release => {
                refuse_release_in_flight(&record)?;
                record.outcome = Outcome::Released;
            }
            Action::Resume { .. } => unreachable!("resume has its own recoverable transition"),
        }
        record.revision = record
            .revision
            .checked_add(1)
            .ok_or("assignment revision exhausted")?;
        let result = record.snapshot();
        record.keep_receipt(request, &result);
        record.pending = None;
        self.save(&record)?;
        Ok(result)
    }

    fn resume_mutation(
        &self,
        mut record: Assignment,
        request: &Mutation,
        new_owner: &str,
    ) -> Result<Snapshot, String> {
        if new_owner.trim().is_empty() {
            return Err("replacement owner is required".into());
        }
        if let Some(receipt) = record
            .receipts
            .iter()
            .find(|r| r.request.request_id == request.request_id)
        {
            return if receipt.request == *request
                && receipt.result.generation == record.generation
                && record.owner == new_owner
            {
                Ok(receipt.result.clone())
            } else {
                Err("stale or changed resume request".into())
            };
        }
        let next_generation = request
            .generation
            .checked_add(1)
            .ok_or("owner generation exhausted")?;
        let recovering = record
            .pending
            .as_ref()
            .is_some_and(|p| p.request == *request)
            && record.owner == new_owner
            && record.generation == next_generation;
        if !recovering && (record.owner != request.owner || record.generation != request.generation)
        {
            return Err("stale assignment owner".into());
        }
        if record.revision != request.revision {
            return Err("assignment revision changed; inspect before recovery".into());
        }
        let observed = crate::kanban::git(&record.checkout, &["branch", "--show-current"])?;
        let mut board = crate::kanban::CardStore::default();
        board.set_dir(Some(&record.board));
        if !record
            .pending
            .as_ref()
            .is_some_and(|p| p.request == *request)
        {
            // Claim repair recognizes predecessors only through interrupted
            // Resumes; any other interrupted request needs no record.
            if let Some(pending) = record
                .pending
                .take()
                .filter(|p| matches!(p.request.action, Action::Resume { .. }))
            {
                record.interrupted.push(pending);
            }
            record.pending = Some(Pending {
                request: request.clone(),
                starting_card: record.current.clone(),
            });
            self.save(&record)?;
        }
        record.owner = new_owner.into();
        record.generation = next_generation;
        record.pause_requested = false;
        // Fence the old owner before repairing the board claim. The persisted
        // resume intent recognizes a retry at this new generation.
        self.save(&record)?;
        let active = record
            .current
            .as_ref()
            .or_else(|| record.transition.as_ref().map(|p| &p.card));
        if let Some(id) = active {
            if let Some(card) = board
                .get(id)
                .filter(|c| c.state == crate::kanban::CardState::InProgress)
            {
                let old = claim_identity(&request.owner);
                let new = claim_identity(new_owner);
                if card.claim.as_ref().is_none_or(|c| {
                    (c.terminal != old.0 || c.run != old.1)
                        && (c.terminal != new.0 || c.run != new.1)
                        && !record
                            .interrupted
                            .iter()
                            .filter(|p| matches!(p.request.action, Action::Resume { .. }))
                            .any(|p| {
                                let predecessor = claim_identity(&p.request.owner);
                                c.terminal == predecessor.0 && c.run == predecessor.1
                            })
                }) {
                    return Err(
                        "card claim belongs to an unexpected owner; inspect before recovery".into(),
                    );
                }
                board.reassign_assignment(id, &new.0, &new.1)?;
            }
        }
        record.outcome = if record.verify_accepted_commits().is_err() {
            Outcome::RecoveryRequired
        } else if let Some(id) = &record.current {
            let queue = crate::integrate::Queue::for_repo(&crate::integrate::repo_identity(
                &record.checkout,
            )?);
            if let Some(queued) = queue
                .get(id)
                .or_else(|| record.evidence.iter().find(|r| r.card == *id).cloned())
            {
                if queued.phase == crate::integrate::Phase::NeedsResolution {
                    Outcome::RecoveryRequired
                } else {
                    Outcome::Waiting
                }
            } else if observed != format!("card/{id}")
                || crate::integrate::git_op_in_progress(&record.checkout).is_some()
            {
                Outcome::RecoveryRequired
            } else if board
                .get(id)
                .is_some_and(|c| c.state == crate::kanban::CardState::Blocked)
            {
                Outcome::Blocked
            } else {
                let (terminal, run) = claim_identity(new_owner);
                board.reassign_assignment(id, &terminal, &run)?;
                Outcome::Work
            }
        } else if record.transition.is_some() {
            Outcome::RecoveryRequired
        } else if observed != record.begin.as_ref().ok_or("missing intent")?.target {
            Outcome::RecoveryRequired
        } else if record
            .approved
            .iter()
            .all(|b| record.evidence.iter().any(|r| r.card == b.id))
        {
            Outcome::Complete
        } else {
            Outcome::Waiting
        };
        record.revision = record
            .revision
            .checked_add(1)
            .ok_or("assignment revision exhausted")?;
        let result = record.snapshot();
        record.keep_receipt(request, &result);
        record.pending = None;
        self.save(&record)?;
        Ok(result)
    }
}

/// Release is the explicit way out of Blocked, ScopeChanged, or an abandoned
/// plan. It changes neither Git nor the board, so it is refused only while an
/// assignment-owned transition could still move them.
fn refuse_release_in_flight(record: &Assignment) -> Result<(), String> {
    if record.transition.is_some() {
        return Err("branch preparation is unfinished; continue or resume before releasing".into());
    }
    let Some(id) = &record.current else {
        return Ok(());
    };
    let queue =
        crate::integrate::Queue::for_repo(&crate::integrate::repo_identity(&record.checkout)?);
    if let Some(request) = queue.get(id) {
        return Err(format!(
            "card {id} has a {} integration request; cancel it or continue to reconcile before releasing",
            request.phase.label()
        ));
    }
    Ok(())
}

fn claim_identity(owner: &str) -> (String, String) {
    match serde_json::from_str::<OwnerIdentity>(owner) {
        Ok(identity) => (identity.terminal, identity.instance),
        Err(_) => (owner.into(), crate::kanban::run_nonce().into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository() -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| crate::kanban::git(repo.path(), args).unwrap();
        git(&["init", "-b", "main"]);
        git(&["config", "user.name", "Assignment Test"]);
        git(&["config", "user.email", "assignment@example.invalid"]);
        std::fs::write(repo.path().join("input.txt"), "committed").unwrap();
        git(&["add", "input.txt"]);
        git(&["commit", "-m", "Fixture"]);
        repo
    }

    #[test]
    fn begin_prepares_untracked_feature_branch_before_claim_and_keeps_dirty_files() {
        let repo = repository();
        let mut board = crate::kanban::CardStore::default();
        board.set_dir(Some(repo.path()));
        let card = board
            .add(
                "Implement first outcome",
                Some("Acceptance: reservation survives restart"),
            )
            .unwrap();
        std::fs::write(repo.path().join("input.txt"), "unrelated dirty work").unwrap();
        std::fs::create_dir(repo.path().join("target")).unwrap();
        std::fs::write(repo.path().join("target/cache"), "warm build").unwrap();
        crate::kanban::git(repo.path(), &["config", "branch.autoSetupMerge", "always"]).unwrap();
        let store =
            AssignmentStore::for_repo(&crate::integrate::repo_identity(repo.path()).unwrap());
        let begin = Begin {
            id: "sequence".into(),
            request_id: "begin-1".into(),
            owner: "instance/project/terminal/provider-session".into(),
            checkout: repo.path().into(),
            target: "main".into(),
            source_plan: None,
            cards: vec![card.clone()],
        };
        let result = store.begin(&begin).unwrap();
        assert_eq!(result.outcome, Outcome::Work);
        assert_eq!(result.current.as_deref(), Some(card.as_str()));
        let branch = format!("card/{card}");
        assert_eq!(result.branch.as_deref(), Some(branch.as_str()));
        assert!(
            result
                .brief
                .as_deref()
                .unwrap()
                .contains("Acceptance: reservation survives restart")
        );
        assert!(result.next.contains("implement"));
        assert_eq!(
            crate::kanban::git(repo.path(), &["branch", "--show-current"]).unwrap(),
            branch
        );
        assert_eq!(
            crate::kanban::git(
                repo.path(),
                &[
                    "for-each-ref",
                    "--format=%(upstream)",
                    &format!("refs/heads/{branch}")
                ]
            )
            .unwrap(),
            ""
        );
        board.reload();
        assert_eq!(
            board.get(&card).unwrap().state,
            crate::kanban::CardState::InProgress
        );
        assert_eq!(
            std::fs::read_to_string(repo.path().join("input.txt")).unwrap(),
            "unrelated dirty work"
        );
        assert_eq!(
            std::fs::read_to_string(repo.path().join("target/cache")).unwrap(),
            "warm build"
        );
        assert_eq!(store.begin(&begin).unwrap(), result);
    }

    #[test]
    fn begin_respects_current_plan_wave_before_creation_order() {
        let repo = repository();
        let mut board = crate::kanban::CardStore::default();
        board.set_dir(Some(repo.path()));
        let later = board.add("Later wave", None).unwrap();
        let first = board.add("First wave", None).unwrap();
        board
            .edit(&later, None, None, Some("Delivery"), Some(2))
            .unwrap();
        board
            .edit(&first, None, None, Some("Delivery"), Some(1))
            .unwrap();
        // Pin timestamps instead of relying on wall-clock ordering.
        for (id, created) in [(&later, "2020-01-01"), (&first, "2020-01-02")] {
            let mut card = board.get(id).unwrap().clone();
            card.created = created.into();
            std::fs::write(
                repo.path().join(format!(".foreman/tasks/{id}.json")),
                serde_json::to_vec(&card).unwrap(),
            )
            .unwrap();
        }
        let store =
            AssignmentStore::for_repo(&crate::integrate::repo_identity(repo.path()).unwrap());
        let result = store
            .begin(&Begin {
                id: "waves".into(),
                request_id: "begin".into(),
                owner: "owner".into(),
                checkout: repo.path().into(),
                target: "main".into(),
                source_plan: None,
                cards: vec![later, first.clone()],
            })
            .unwrap();
        assert_eq!(result.current, Some(first));
    }

    #[test]
    fn continuation_waits_for_verified_integration_before_claiming_the_next_card() {
        let repo = repository();
        let mut board = crate::kanban::CardStore::default();
        board.set_dir(Some(repo.path()));
        let a = board.add("Outcome A", None).unwrap();
        let b = board.add("Outcome B", None).unwrap();
        let common = crate::integrate::repo_identity(repo.path()).unwrap();
        let store = AssignmentStore::for_repo(&common);
        let mut result = store
            .begin(&Begin {
                id: "sequence".into(),
                request_id: "begin".into(),
                owner: "owner".into(),
                checkout: repo.path().into(),
                target: "main".into(),
                source_plan: None,
                cards: vec![a.clone(), b.clone()],
            })
            .unwrap();
        let first = result.current.clone().unwrap();
        let second = if first == a { b } else { a };
        std::fs::write(repo.path().join("outcome.txt"), "accepted work").unwrap();
        crate::kanban::git(repo.path(), &["add", "outcome.txt"]).unwrap();
        crate::kanban::git(repo.path(), &["commit", "-m", "Implement outcome"]).unwrap();
        let commit = crate::kanban::git(repo.path(), &["rev-parse", "HEAD"]).unwrap();
        let request = Mutation {
            request_id: "submit".into(),
            owner: "owner".into(),
            generation: result.generation,
            revision: result.revision,
            action: Action::Continue { submit: true },
        };
        result = store.mutate("sequence", &request).unwrap();
        assert_eq!(result.outcome, Outcome::Waiting);
        assert_eq!(result.current.as_deref(), Some(first.as_str()));
        board.reload();
        assert_eq!(
            board.get(&second).unwrap().state,
            crate::kanban::CardState::Backlog
        );
        let queue = crate::integrate::Queue::for_repo(&common);
        crate::integrate::run_turn(
            &queue,
            &crate::integrate::Owner::this_process(),
            &crate::integrate::NoHooks,
        )
        .unwrap();
        // The GUI may finish board bookkeeping before the worker polls again.
        queue.remove(&first).unwrap();
        // A replay cannot advance even though the queue finished since the first response.
        assert_eq!(store.mutate("sequence", &request).unwrap(), result);
        result = store
            .mutate(
                "sequence",
                &Mutation {
                    request_id: "advance".into(),
                    revision: result.revision,
                    action: Action::Continue { submit: false },
                    ..request
                },
            )
            .unwrap();
        assert_eq!(result.outcome, Outcome::Work);
        assert_eq!(result.current, Some(second.clone()));
        board.reload();
        assert_eq!(
            board.get(&first).unwrap().state,
            crate::kanban::CardState::Done
        );
        assert_eq!(
            crate::kanban::git(repo.path(), &["branch", "--show-current"]).unwrap(),
            format!("card/{second}")
        );
        assert_eq!(store.get("sequence").unwrap().evidence[0].commit, commit);
    }

    #[test]
    fn pause_survives_integration_and_resume_fences_the_old_owner_until_completion() {
        let repo = repository();
        let mut board = crate::kanban::CardStore::default();
        board.set_dir(Some(repo.path()));
        let card = board.add("Only outcome", None).unwrap();
        let common = crate::integrate::repo_identity(repo.path()).unwrap();
        let store = AssignmentStore::for_repo(&common);
        let begun = store
            .begin(&Begin {
                id: "sequence".into(),
                request_id: "begin".into(),
                owner: "owner".into(),
                checkout: repo.path().into(),
                target: "main".into(),
                source_plan: None,
                cards: vec![card],
            })
            .unwrap();
        let pause = Mutation {
            request_id: "pause".into(),
            owner: "owner".into(),
            generation: begun.generation,
            revision: begun.revision,
            action: Action::Pause,
        };
        let paused = store.mutate("sequence", &pause).unwrap();
        assert_eq!(paused.outcome, Outcome::Work);
        let submitted = store
            .mutate(
                "sequence",
                &Mutation {
                    request_id: "submit".into(),
                    revision: paused.revision,
                    action: Action::Continue { submit: true },
                    ..pause.clone()
                },
            )
            .unwrap();
        crate::integrate::run_turn(
            &crate::integrate::Queue::for_repo(&common),
            &crate::integrate::Owner::this_process(),
            &crate::integrate::NoHooks,
        )
        .unwrap();
        let paused = store
            .mutate(
                "sequence",
                &Mutation {
                    request_id: "after".into(),
                    revision: submitted.revision,
                    action: Action::Continue { submit: false },
                    ..pause.clone()
                },
            )
            .unwrap();
        assert_eq!(paused.outcome, Outcome::Paused);
        assert!(store.reserve("competitor", repo.path(), "other").is_err());
        let resume = Mutation {
            request_id: "resume".into(),
            revision: paused.revision,
            action: Action::Resume {
                new_owner: "replacement".into(),
            },
            ..pause.clone()
        };
        let resumed = store.mutate("sequence", &resume).unwrap();
        assert_eq!(store.mutate("sequence", &resume).unwrap(), resumed);
        assert_eq!(resumed.outcome, Outcome::Complete);
        assert_eq!(resumed.generation, begun.generation + 1);
        assert!(store.mutate("sequence", &pause).is_err());
        assert!(
            store
                .reserve("next-assignment", repo.path(), "replacement")
                .is_ok()
        );
    }

    #[test]
    fn waiting_assignment_observes_changed_brief_without_expanding_approved_scope() {
        let repo = repository();
        let mut board = crate::kanban::CardStore::default();
        board.set_dir(Some(repo.path()));
        let prerequisite = board.add("External prerequisite", None).unwrap();
        let selected = board
            .add("Approved outcome", Some("Original acceptance"))
            .unwrap();
        board
            .edit(&prerequisite, None, None, Some("Plan"), Some(1))
            .unwrap();
        board
            .edit(&selected, None, None, Some("Plan"), Some(2))
            .unwrap();
        let store =
            AssignmentStore::for_repo(&crate::integrate::repo_identity(repo.path()).unwrap());
        let result = store
            .begin(&Begin {
                id: "subset".into(),
                request_id: "begin".into(),
                owner: "owner".into(),
                checkout: repo.path().into(),
                target: "main".into(),
                source_plan: None,
                cards: vec![selected.clone()],
            })
            .unwrap();
        assert_eq!(result.outcome, Outcome::Waiting);
        assert!(result.current.is_none());
        // A different checkout finishes the prerequisite while this one is reserved.
        let mut done = board.get(&prerequisite).unwrap().clone();
        done.state = crate::kanban::CardState::Done;
        std::fs::write(
            repo.path()
                .join(format!(".foreman/tasks/{prerequisite}.json")),
            serde_json::to_vec(&done).unwrap(),
        )
        .unwrap();
        board
            .edit(&selected, None, Some("Expanded acceptance"), None, None)
            .unwrap();
        let result = store
            .mutate(
                "subset",
                &Mutation {
                    request_id: "continue".into(),
                    owner: "owner".into(),
                    generation: result.generation,
                    revision: result.revision,
                    action: Action::Continue { submit: false },
                },
            )
            .unwrap();
        assert_eq!(result.outcome, Outcome::ScopeChanged);
        board.reload();
        assert_eq!(
            board.get(&selected).unwrap().state,
            crate::kanban::CardState::Backlog
        );
        assert_eq!(
            crate::kanban::git(repo.path(), &["branch", "--show-current"]).unwrap(),
            "main"
        );
    }

    #[test]
    fn legacy_claim_and_branch_preparation_cannot_take_a_reserved_checkout() {
        let repo = repository();
        let mut board = crate::kanban::CardStore::default();
        board.set_dir(Some(repo.path()));
        let card = board.add("Competing work", None).unwrap();
        let store =
            AssignmentStore::for_repo(&crate::integrate::repo_identity(repo.path()).unwrap());
        store.reserve("reserved", repo.path(), "owner").unwrap();
        assert!(
            board
                .start(&card, "other", "run", crate::kanban::TermState::Missing)
                .is_err()
        );
        assert!(crate::kanban::bring_up_branch(repo.path(), board.get(&card).unwrap()).is_err());
        assert!(
            board
                .claim_for_dispatch(
                    &card,
                    "other",
                    "codex",
                    "run",
                    crate::kanban::TermState::Missing,
                    None
                )
                .is_err()
        );
        assert_eq!(
            crate::kanban::git(repo.path(), &["branch", "--show-current"]).unwrap(),
            "main"
        );
    }

    #[test]
    fn legacy_closeout_and_teardown_cannot_release_an_assignment_card() {
        let repo = repository();
        let mut board = crate::kanban::CardStore::default();
        board.set_dir(Some(repo.path()));
        let card = board.add("Assignment outcome", None).unwrap();
        let store =
            AssignmentStore::for_repo(&crate::integrate::repo_identity(repo.path()).unwrap());
        store
            .begin(&Begin {
                id: "sequence".into(),
                request_id: "begin".into(),
                owner: "owner".into(),
                checkout: repo.path().into(),
                target: "main".into(),
                source_plan: None,
                cards: vec![card.clone()],
            })
            .unwrap();
        board.reload();
        assert!(board.release(&card).is_err());
        assert!(board.done(&card).is_err());
        assert!(board.rm(&card).is_err());
        let wt = board.get(&card).unwrap().worktree.clone().unwrap();
        assert!(matches!(
            crate::kanban::teardown_worktree(repo.path(), &wt, false),
            crate::kanban::TeardownOutcome::Held(_)
        ));
        assert_eq!(
            crate::kanban::git(repo.path(), &["branch", "--show-current"]).unwrap(),
            format!("card/{card}")
        );
    }

    #[test]
    fn continue_revalidates_the_active_checkout_before_returning_work() {
        let repo = repository();
        let mut board = crate::kanban::CardStore::default();
        board.set_dir(Some(repo.path()));
        let card = board.add("Outcome", None).unwrap();
        let store =
            AssignmentStore::for_repo(&crate::integrate::repo_identity(repo.path()).unwrap());
        let result = store
            .begin(&Begin {
                id: "sequence".into(),
                request_id: "begin".into(),
                owner: "owner".into(),
                checkout: repo.path().into(),
                target: "main".into(),
                source_plan: None,
                cards: vec![card.clone()],
            })
            .unwrap();
        crate::kanban::git(repo.path(), &["switch", "main"]).unwrap();
        assert!(
            store
                .mutate(
                    "sequence",
                    &Mutation {
                        request_id: "continue".into(),
                        owner: "owner".into(),
                        generation: result.generation,
                        revision: result.revision,
                        action: Action::Continue { submit: false }
                    }
                )
                .is_err()
        );
        assert_eq!(store.get("sequence").unwrap().current, Some(card));
    }

    #[test]
    fn subdirectory_projects_reserve_the_git_checkout_and_keep_their_board_location() {
        let repo = repository();
        let project = repo.path().join("subproject");
        std::fs::create_dir(&project).unwrap();
        let mut board = crate::kanban::CardStore::default();
        board.set_dir(Some(&project));
        let card = board.add("Subproject outcome", None).unwrap();
        let store =
            AssignmentStore::for_repo(&crate::integrate::repo_identity(repo.path()).unwrap());
        let result = store
            .begin(&Begin {
                id: "sequence".into(),
                request_id: "begin".into(),
                owner: "owner".into(),
                checkout: project,
                target: "main".into(),
                source_plan: None,
                cards: vec![card],
            })
            .unwrap();
        assert!(store.reserve("competing", repo.path(), "other").is_err());
        assert!(guard_checkout(repo.path()).is_err());
        let result = store
            .mutate(
                "sequence",
                &Mutation {
                    request_id: "continue".into(),
                    owner: "owner".into(),
                    generation: result.generation,
                    revision: result.revision,
                    action: Action::Continue { submit: false },
                },
            )
            .unwrap();
        assert_eq!(result.outcome, Outcome::Work);
    }

    #[test]
    fn recovery_context_is_read_only_owner_scoped_and_reports_observed_git_disagreement() {
        let repo = repository();
        let owner = OwnerIdentity {
            instance: "run".into(),
            project: "p1".into(),
            terminal: "t1".into(),
            provider: "codex".into(),
            session: "root-session".into(),
            process: crate::proc::ProviderProcess {
                pid: 10,
                started: 100,
            },
        };
        let owner_key = serde_json::to_string(&owner).unwrap();
        let mut board = crate::kanban::CardStore::default();
        board.set_dir(Some(repo.path()));
        let card = board
            .add("Recover this outcome", Some("Keep unrelated work"))
            .unwrap();
        let store =
            AssignmentStore::for_repo(&crate::integrate::repo_identity(repo.path()).unwrap());
        store
            .begin(&Begin {
                id: "sequence".into(),
                request_id: "begin".into(),
                owner: owner_key.clone(),
                checkout: repo.path().into(),
                target: "main".into(),
                source_plan: None,
                cards: vec![card.clone()],
            })
            .unwrap();
        let before = store.get("sequence").unwrap();
        crate::kanban::git(repo.path(), &["switch", "main"]).unwrap();
        let brief = store.recovery_brief(&owner_key).unwrap().unwrap();
        assert!(brief.contains(&format!("card/{card}")));
        assert!(brief.contains("observed branch: main"));
        assert!(brief.contains("disagreement"));
        assert_eq!(store.recovery_brief(&owner_key).unwrap(), Some(brief));
        let mut unrelated = Vec::new();
        for session in ["forked-session", "replacement-session"] {
            unrelated.push(OwnerIdentity {
                session: session.into(),
                ..owner.clone()
            });
        }
        unrelated.push(OwnerIdentity {
            process: crate::proc::ProviderProcess {
                pid: 10,
                started: 200,
            },
            ..owner.clone()
        });
        unrelated.push(OwnerIdentity {
            process: crate::proc::ProviderProcess {
                pid: 20,
                started: 100,
            },
            ..owner.clone()
        });
        unrelated.push(OwnerIdentity {
            instance: "other-run".into(),
            ..owner.clone()
        });
        unrelated.push(OwnerIdentity {
            project: "p2".into(),
            ..owner.clone()
        });
        unrelated.push(OwnerIdentity {
            terminal: "t2".into(),
            ..owner.clone()
        });
        unrelated.push(OwnerIdentity {
            provider: "claude".into(),
            ..owner.clone()
        });
        for other in unrelated {
            assert!(
                store
                    .recovery_brief(&serde_json::to_string(&other).unwrap())
                    .unwrap()
                    .is_none(),
                "unrelated owner received context: {other:?}"
            );
        }
        assert_eq!(store.get("sequence").unwrap(), before);
    }

    #[test]
    fn replacement_command_recovers_a_pending_request_and_replays_its_own_response() {
        let repo = repository();
        let mut board = crate::kanban::CardStore::default();
        board.set_dir(Some(repo.path()));
        let card = board.add("Recover pending work", None).unwrap();
        let owner = OwnerIdentity {
            instance: "run".into(),
            project: "p1".into(),
            terminal: "t1".into(),
            provider: "codex".into(),
            session: "session-a".into(),
            process: crate::proc::ProviderProcess {
                pid: 1,
                started: 10,
            },
        };
        let command = Command {
            operation: "begin".into(),
            id: "sequence".into(),
            request_id: "begin".into(),
            provider: "codex".into(),
            session: owner.session.clone(),
            caller_pid: 2,
            target: "main".into(),
            cards: vec![card.clone()],
            ..Default::default()
        };
        let begun = command.execute(repo.path(), &owner).unwrap();
        board.reload();
        assert_eq!(
            board.get(&card).unwrap().claim.as_ref().unwrap().terminal,
            "t1"
        );
        crate::kanban::git(repo.path(), &["switch", "main"]).unwrap();
        let pending = Command {
            operation: "continue".into(),
            request_id: "pending".into(),
            generation: Some(begun.generation),
            revision: Some(begun.revision),
            ..command.clone()
        };
        assert!(pending.execute(repo.path(), &owner).is_err());
        crate::kanban::git(repo.path(), &["switch", &format!("card/{card}")]).unwrap();
        let replacement = OwnerIdentity {
            terminal: "t2".into(),
            session: "session-b".into(),
            ..owner.clone()
        };
        let resume = Command {
            operation: "resume".into(),
            request_id: "resume".into(),
            recover: true,
            session: replacement.session.clone(),
            ..pending.clone()
        };
        let result = resume.execute(repo.path(), &replacement).unwrap();
        assert_eq!(result.outcome, Outcome::Work);
        assert_eq!(resume.execute(repo.path(), &replacement).unwrap(), result);
        assert!(pending.execute(repo.path(), &owner).is_err());
        board.reload();
        assert_eq!(
            board.get(&card).unwrap().claim.as_ref().unwrap().terminal,
            "t2"
        );
    }

    #[test]
    fn replacement_can_inspect_without_changing_ownership() {
        let repo = repository();
        let store =
            AssignmentStore::for_repo(&crate::integrate::repo_identity(repo.path()).unwrap());
        let before = store.reserve("sequence", repo.path(), "old-owner").unwrap();
        let owner = OwnerIdentity {
            instance: "run".into(),
            project: "p1".into(),
            terminal: "t2".into(),
            provider: "codex".into(),
            session: "new".into(),
            process: crate::proc::ProviderProcess {
                pid: 2,
                started: 20,
            },
        };
        let command = Command {
            operation: "status".into(),
            id: "sequence".into(),
            provider: "codex".into(),
            session: "new".into(),
            caller_pid: 3,
            ..Default::default()
        };
        let status = command.execute(repo.path(), &owner).unwrap();
        assert_eq!(status.outcome, Outcome::RecoveryRequired);
        assert_eq!(store.get("sequence").unwrap(), before);
    }

    #[test]
    fn replacement_command_recovers_after_every_ownership_write() {
        for cut in 1..=3 {
            let repo = repository();
            let mut board = crate::kanban::CardStore::default();
            board.set_dir(Some(repo.path()));
            let card = board.add("Recover ownership", None).unwrap();
            let owner = OwnerIdentity {
                instance: "run".into(),
                project: "p1".into(),
                terminal: "t1".into(),
                provider: "codex".into(),
                session: "a".into(),
                process: crate::proc::ProviderProcess {
                    pid: 1,
                    started: 10,
                },
            };
            let begin = Command {
                operation: "begin".into(),
                id: "sequence".into(),
                request_id: "begin".into(),
                provider: "codex".into(),
                session: "a".into(),
                caller_pid: 2,
                target: "main".into(),
                cards: vec![card.clone()],
                ..Default::default()
            };
            let result = begin.execute(repo.path(), &owner).unwrap();
            let replacement = OwnerIdentity {
                terminal: "t2".into(),
                session: "b".into(),
                ..owner.clone()
            };
            let resume = Command {
                operation: "resume".into(),
                request_id: "resume".into(),
                generation: Some(result.generation),
                revision: Some(result.revision),
                session: "b".into(),
                recover: true,
                ..begin.clone()
            };
            INTERRUPT_AFTER_SAVE.with(|n| n.set(Some(cut)));
            assert!(resume.execute(repo.path(), &replacement).is_err());
            INTERRUPT_AFTER_SAVE.with(|n| n.set(None));
            let result = resume.execute(repo.path(), &replacement).unwrap();
            assert_eq!(result.generation, 2);
            assert_eq!(resume.execute(repo.path(), &replacement).unwrap(), result);
            assert!(begin.execute(repo.path(), &owner).is_err());
            board.reload();
            assert_eq!(
                board.get(&card).unwrap().claim.as_ref().unwrap().terminal,
                "t2"
            );
        }
    }

    #[test]
    fn failed_integration_can_be_resubmitted_after_replacement_and_interruption() {
        for replace in [true, false] {
            let repo = repository();
            let mut board = crate::kanban::CardStore::default();
            board.set_dir(Some(repo.path()));
            let card = board.add("Repair and resubmit", None).unwrap();
            let common = crate::integrate::repo_identity(repo.path()).unwrap();
            let store = AssignmentStore::for_repo(&common);
            let mut result = store
                .begin(&Begin {
                    id: "sequence".into(),
                    request_id: "begin".into(),
                    owner: "old".into(),
                    checkout: repo.path().into(),
                    target: "main".into(),
                    source_plan: None,
                    cards: vec![card.clone()],
                })
                .unwrap();
            std::fs::write(
                repo.path().join(crate::integrate::POLICY_FILE),
                r#"{"check":["git","cat-file","-e","HEAD:missing"]}"#,
            )
            .unwrap();
            let mut request = Mutation {
                request_id: "submit".into(),
                owner: "old".into(),
                generation: result.generation,
                revision: result.revision,
                action: Action::Continue { submit: true },
            };
            result = store.mutate("sequence", &request).unwrap();
            let queue = crate::integrate::Queue::for_repo(&common);
            crate::integrate::run_turn(
                &queue,
                &crate::integrate::Owner::this_process(),
                &crate::integrate::NoHooks,
            )
            .unwrap();
            assert_eq!(
                queue.get(&card).unwrap().phase,
                crate::integrate::Phase::NeedsResolution
            );
            if replace {
                result = store
                    .mutate(
                        "sequence",
                        &Mutation {
                            request_id: "resume".into(),
                            revision: result.revision,
                            action: Action::Resume {
                                new_owner: "new".into(),
                            },
                            ..request.clone()
                        },
                    )
                    .unwrap();
                request.owner = "new".into();
            }
            std::fs::write(repo.path().join("missing"), "fixed").unwrap();
            crate::kanban::git(repo.path(), &["add", "missing"]).unwrap();
            crate::kanban::git(repo.path(), &["commit", "-m", "Repair check"]).unwrap();
            request.request_id = "resubmit".into();
            request.generation = result.generation;
            request.revision = result.revision;
            if !replace {
                INTERRUPT_AFTER_SAVE.with(|n| n.set(Some(2)));
                assert!(store.mutate("sequence", &request).is_err());
                INTERRUPT_AFTER_SAVE.with(|n| n.set(None));
            }
            let result = store.mutate("sequence", &request).unwrap();
            assert_eq!(result.outcome, Outcome::Waiting);
            assert_eq!(
                queue.get(&card).unwrap().commit,
                crate::kanban::git(repo.path(), &["rev-parse", "HEAD"]).unwrap()
            );
        }
    }

    #[test]
    fn replacement_recovers_a_claim_written_before_preparation_finished() {
        let repo = repository();
        let mut board = crate::kanban::CardStore::default();
        board.set_dir(Some(repo.path()));
        let card = board.add("Interrupted preparation", None).unwrap();
        let store =
            AssignmentStore::for_repo(&crate::integrate::repo_identity(repo.path()).unwrap());
        let begin = Begin {
            id: "sequence".into(),
            request_id: "begin".into(),
            owner: "old".into(),
            checkout: repo.path().into(),
            target: "main".into(),
            cards: vec![card.clone()],
            source_plan: None,
        };
        store.begin(&begin).unwrap();
        let mut interrupted = store.get("sequence").unwrap();
        interrupted.current = None;
        interrupted.begin_result = None;
        interrupted.transition = Some(Preparation {
            card,
            base_commit: crate::kanban::git(repo.path(), &["rev-parse", "HEAD"]).unwrap(),
        });
        store.save(&interrupted).unwrap();
        let request = Mutation {
            request_id: "resume".into(),
            owner: "old".into(),
            generation: 1,
            revision: 1,
            action: Action::Resume {
                new_owner: "new".into(),
            },
        };
        let resumed = store.mutate("sequence", &request).unwrap();
        let result = store
            .mutate(
                "sequence",
                &Mutation {
                    request_id: "continue".into(),
                    owner: "new".into(),
                    generation: resumed.generation,
                    revision: resumed.revision,
                    action: Action::Continue { submit: false },
                },
            )
            .unwrap();
        assert_eq!(result.outcome, Outcome::Work);
    }

    fn begun() -> (
        tempfile::TempDir,
        crate::kanban::CardStore,
        AssignmentStore,
        Begin,
        Snapshot,
    ) {
        let repo = repository();
        let mut board = crate::kanban::CardStore::default();
        board.set_dir(Some(repo.path()));
        let card = board.add("Outcome", Some("Approved acceptance")).unwrap();
        let store =
            AssignmentStore::for_repo(&crate::integrate::repo_identity(repo.path()).unwrap());
        let begin = Begin {
            id: "sequence".into(),
            request_id: "begin".into(),
            owner: "owner".into(),
            checkout: repo.path().into(),
            target: "main".into(),
            source_plan: None,
            cards: vec![card],
        };
        let result = store.begin(&begin).unwrap();
        (repo, board, store, begin, result)
    }

    #[test]
    fn review_blocked_card_cannot_resume_execution_or_submit() {
        let (repo, mut board, store, begin, mut result) = begun();
        board.reload();
        board.block(&begin.cards[0], "Unclear acceptance").unwrap();
        for (request_id, submit) in [("observe", false), ("submit", true)] {
            result = store
                .mutate(
                    &begin.id,
                    &Mutation {
                        request_id: request_id.into(),
                        owner: begin.owner.clone(),
                        generation: result.generation,
                        revision: result.revision,
                        action: Action::Continue { submit },
                    },
                )
                .unwrap();
            assert_eq!(result.outcome, Outcome::Blocked);
        }
        assert!(
            crate::integrate::Queue::for_repo(
                &crate::integrate::repo_identity(repo.path()).unwrap()
            )
            .get(&begin.cards[0])
            .is_none()
        );
    }

    #[test]
    fn review_recovered_preparation_rechecks_scope_before_claiming() {
        let repo = repository();
        let mut board = crate::kanban::CardStore::default();
        board.set_dir(Some(repo.path()));
        let card = board.add("Outcome", Some("Approved acceptance")).unwrap();
        let store =
            AssignmentStore::for_repo(&crate::integrate::repo_identity(repo.path()).unwrap());
        let begin = Begin {
            id: "sequence".into(),
            request_id: "begin".into(),
            owner: "owner".into(),
            checkout: repo.path().into(),
            target: "main".into(),
            source_plan: None,
            cards: vec![card.clone()],
        };
        INTERRUPT_AFTER_SAVE.with(|n| n.set(Some(2)));
        assert!(store.begin(&begin).is_err());
        INTERRUPT_AFTER_SAVE.with(|n| n.set(None));
        board
            .edit(&card, None, Some("Expanded scope"), None, None)
            .unwrap();
        assert_eq!(store.begin(&begin).unwrap().outcome, Outcome::ScopeChanged);
        board.reload();
        assert_eq!(
            board.get(&card).unwrap().state,
            crate::kanban::CardState::Backlog
        );
    }

    #[test]
    fn review_begin_replay_is_fenced_after_owner_recovery() {
        let (_repo, _board, store, begin, result) = begun();
        store
            .mutate(
                &begin.id,
                &Mutation {
                    request_id: "resume".into(),
                    owner: begin.owner.clone(),
                    generation: result.generation,
                    revision: result.revision,
                    action: Action::Resume {
                        new_owner: "replacement".into(),
                    },
                },
            )
            .unwrap();
        assert!(store.begin(&begin).is_err());
    }

    #[test]
    fn review_completion_revalidates_the_checkout_before_releasing_it() {
        let (repo, _board, store, begin, result) = begun();
        let result = store
            .mutate(
                &begin.id,
                &Mutation {
                    request_id: "submit".into(),
                    owner: begin.owner.clone(),
                    generation: result.generation,
                    revision: result.revision,
                    action: Action::Continue { submit: true },
                },
            )
            .unwrap();
        let queue = crate::integrate::Queue::for_repo(
            &crate::integrate::repo_identity(repo.path()).unwrap(),
        );
        crate::integrate::run_turn(
            &queue,
            &crate::integrate::Owner::this_process(),
            &crate::integrate::NoHooks,
        )
        .unwrap();
        crate::kanban::git(repo.path(), &["switch", "--no-track", "-c", "unexpected"]).unwrap();
        assert!(
            store
                .mutate(
                    &begin.id,
                    &Mutation {
                        request_id: "continue".into(),
                        owner: begin.owner.clone(),
                        generation: result.generation,
                        revision: result.revision,
                        action: Action::Continue { submit: false }
                    }
                )
                .is_err()
        );
        assert!(store.reserve("competing", repo.path(), "other").is_err());
    }

    #[test]
    fn legacy_claim_in_another_project_board_prevents_assignment_begin() {
        let repo = repository();
        let mut legacy = crate::kanban::CardStore::default();
        legacy.set_dir(Some(repo.path()));
        let active = legacy.add("Existing writer", None).unwrap();
        legacy
            .start(&active, "t1", "run", crate::kanban::TermState::Missing)
            .unwrap();
        let project = repo.path().join("subproject");
        std::fs::create_dir(&project).unwrap();
        let mut board = crate::kanban::CardStore::default();
        board.set_dir(Some(&project));
        let card = board.add("New writer", None).unwrap();
        let store =
            AssignmentStore::for_repo(&crate::integrate::repo_identity(repo.path()).unwrap());
        assert!(
            store
                .begin(&Begin {
                    id: "sequence".into(),
                    request_id: "begin".into(),
                    owner: "owner".into(),
                    checkout: project,
                    target: "main".into(),
                    source_plan: None,
                    cards: vec![card]
                })
                .is_err()
        );
        assert_eq!(
            crate::kanban::git(repo.path(), &["branch", "--show-current"]).unwrap(),
            "main"
        );
    }

    #[test]
    fn integration_cannot_advance_a_destination_reserved_by_another_assignment() {
        let repo = repository();
        let common = crate::integrate::repo_identity(repo.path()).unwrap();
        let card =
            crate::kanban::Card::new("other".into(), "Other work".into(), None, "now".into());
        let crate::kanban::BringUp::Worktree(wt) =
            crate::kanban::bring_up_worktree(repo.path(), &card).unwrap()
        else {
            panic!("worktree required")
        };
        let queue = crate::integrate::Queue::for_repo(&common);
        queue
            .submit(crate::integrate::prepare_submission(repo.path(), &card.id, &wt, None).unwrap())
            .unwrap();
        AssignmentStore::for_repo(&common)
            .reserve("reserved", repo.path(), "owner")
            .unwrap();
        crate::integrate::run_turn(
            &queue,
            &crate::integrate::Owner::this_process(),
            &crate::integrate::NoHooks,
        )
        .unwrap();
        let request = queue.get(&card.id).unwrap();
        assert_eq!(request.phase, crate::integrate::Phase::Queued);
        assert!(request.hold.unwrap().contains("reserved"));
    }

    #[test]
    fn an_approved_future_card_cannot_be_prepared_in_a_legacy_worktree() {
        let repo = repository();
        let mut board = crate::kanban::CardStore::default();
        board.set_dir(Some(repo.path()));
        let id = board.add("Future", None).unwrap();
        let store =
            AssignmentStore::for_repo(&crate::integrate::repo_identity(repo.path()).unwrap());
        let mut record = store.reserve("sequence", repo.path(), "owner").unwrap();
        record.approved.push(Brief::from(board.get(&id).unwrap()));
        store.save(&record).unwrap();
        assert!(crate::kanban::bring_up_worktree(repo.path(), board.get(&id).unwrap()).is_err());
    }

    #[test]
    fn begin_does_not_take_a_checkout_with_pending_legacy_integration() {
        let repo = repository();
        let mut board = crate::kanban::CardStore::default();
        board.set_dir(Some(repo.path()));
        let other = board.add("Other", None).unwrap();
        let crate::kanban::BringUp::Worktree(wt) =
            crate::kanban::bring_up_worktree(repo.path(), board.get(&other).unwrap()).unwrap()
        else {
            panic!("worktree")
        };
        let common = crate::integrate::repo_identity(repo.path()).unwrap();
        crate::integrate::Queue::for_repo(&common)
            .submit(crate::integrate::prepare_submission(repo.path(), &other, &wt, None).unwrap())
            .unwrap();
        let id = board.add("Sequential", None).unwrap();
        let store = AssignmentStore::for_repo(&common);
        let result = store.begin(&Begin {
            id: "sequence".into(),
            request_id: "begin".into(),
            owner: "owner".into(),
            checkout: repo.path().into(),
            target: "main".into(),
            cards: vec![id],
            source_plan: None,
        });
        assert!(
            result.is_err(),
            "pending legacy integration must finish before reserving its destination"
        );
    }

    #[test]
    fn begin_retry_recovers_after_each_durable_write() {
        for interruption in 1..=5 {
            let repo = repository();
            let mut board = crate::kanban::CardStore::default();
            board.set_dir(Some(repo.path()));
            let card = board.add("Recoverable outcome", None).unwrap();
            let store =
                AssignmentStore::for_repo(&crate::integrate::repo_identity(repo.path()).unwrap());
            let request = Begin {
                id: "sequence".into(),
                request_id: "begin".into(),
                owner: "owner".into(),
                checkout: repo.path().into(),
                target: "main".into(),
                source_plan: None,
                cards: vec![card.clone()],
            };
            INTERRUPT_AFTER_SAVE.with(|n| n.set(Some(interruption)));
            let _lost_response = store.begin(&request);
            INTERRUPT_AFTER_SAVE.with(|n| n.set(None));
            let result = store
                .begin(&request)
                .unwrap_or_else(|e| panic!("write {interruption}: {e}"));
            assert_eq!(result.current, Some(card));
            assert_eq!(result.outcome, Outcome::Work);
        }
    }

    #[test]
    fn interrupted_continuation_does_not_submit_the_next_cards_unfinished_work() {
        for interruption in 1..=8 {
            let repo = repository();
            let mut board = crate::kanban::CardStore::default();
            board.set_dir(Some(repo.path()));
            let a = board.add("Outcome A", None).unwrap();
            let b = board.add("Outcome B", None).unwrap();
            let common = crate::integrate::repo_identity(repo.path()).unwrap();
            let store = AssignmentStore::for_repo(&common);
            let begun = store
                .begin(&Begin {
                    id: "sequence".into(),
                    request_id: "begin".into(),
                    owner: "owner".into(),
                    checkout: repo.path().into(),
                    target: "main".into(),
                    source_plan: None,
                    cards: vec![a.clone(), b.clone()],
                })
                .unwrap();
            let next = if begun.current.as_ref() == Some(&a) {
                b
            } else {
                a
            };
            let submitted = store
                .mutate(
                    "sequence",
                    &Mutation {
                        request_id: "submit".into(),
                        owner: "owner".into(),
                        generation: 1,
                        revision: begun.revision,
                        action: Action::Continue { submit: true },
                    },
                )
                .unwrap();
            let queue = crate::integrate::Queue::for_repo(&common);
            crate::integrate::run_turn(
                &queue,
                &crate::integrate::Owner::this_process(),
                &crate::integrate::NoHooks,
            )
            .unwrap();
            let request = Mutation {
                request_id: "advance".into(),
                owner: "owner".into(),
                generation: 1,
                revision: submitted.revision,
                action: Action::Continue { submit: true },
            };
            INTERRUPT_AFTER_SAVE.with(|n| n.set(Some(interruption)));
            let _lost_response = store.mutate("sequence", &request);
            INTERRUPT_AFTER_SAVE.with(|n| n.set(None));
            let result = store
                .mutate("sequence", &request)
                .unwrap_or_else(|e| panic!("write {interruption}: {e}"));
            assert_eq!(result.current, Some(next.clone()), "write {interruption}");
            assert_eq!(result.outcome, Outcome::Work, "write {interruption}");
            assert!(
                queue.get(&next).is_none(),
                "write {interruption} submitted the next card"
            );
        }
    }

    #[test]
    fn reservation_survives_reopening_and_rejects_a_competing_owner() {
        let repo = tempfile::tempdir().unwrap();
        let checkout = repo.path().join("checkout");
        std::fs::create_dir(&checkout).unwrap();
        let store = AssignmentStore::for_repo(repo.path());
        let first = store.reserve("first", &checkout, "owner-a").unwrap();
        drop(store);
        let reopened = AssignmentStore::for_repo(repo.path());
        assert_eq!(reopened.get("first").unwrap().owner, "owner-a");
        assert!(reopened.reserve("second", &checkout, "owner-b").is_err());
        assert_eq!(reopened.get("first").unwrap(), first);
    }

    #[test]
    fn completed_assignments_cannot_reacquire_a_checkout_owned_by_a_successor() {
        let repo = repository();
        let store =
            AssignmentStore::for_repo(&crate::integrate::repo_identity(repo.path()).unwrap());
        let mut complete = store.reserve("old", repo.path(), "old-owner").unwrap();
        complete.outcome = Outcome::Complete;
        store.save(&complete).unwrap();
        store.reserve("new", repo.path(), "new-owner").unwrap();
        let request = Mutation {
            request_id: "resume".into(),
            owner: "old-owner".into(),
            generation: 1,
            revision: 1,
            action: Action::Resume {
                new_owner: "replacement".into(),
            },
        };
        assert!(store.mutate("old", &request).is_err());
        assert_eq!(
            store.get("old").unwrap(),
            complete,
            "completed ownership must stay released even on recovery errors"
        );
    }

    #[test]
    fn reservation_is_respected_by_an_independent_process() {
        const PROBE: &str = "FOREMAN_TEST_ASSIGNMENT_CHECKOUT";
        if let Some(path) = std::env::var_os(PROBE) {
            let checkout = PathBuf::from(path);
            let store =
                AssignmentStore::for_repo(&crate::integrate::repo_identity(&checkout).unwrap());
            assert!(
                store
                    .reserve("competing", &checkout, "other-process")
                    .is_err()
            );
            return;
        }
        let repo = repository();
        let store =
            AssignmentStore::for_repo(&crate::integrate::repo_identity(repo.path()).unwrap());
        store.reserve("owner", repo.path(), "parent").unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "assignments::tests::reservation_is_respected_by_an_independent_process",
            ])
            .env(PROBE, repo.path())
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn another_replacement_can_finish_an_interrupted_owner_transfer() {
        let repo = repository();
        let mut board = crate::kanban::CardStore::default();
        board.set_dir(Some(repo.path()));
        let card = board.add("Survive two replacements", None).unwrap();
        let store =
            AssignmentStore::for_repo(&crate::integrate::repo_identity(repo.path()).unwrap());
        store
            .begin(&Begin {
                id: "sequence".into(),
                request_id: "begin".into(),
                owner: "A".into(),
                checkout: repo.path().into(),
                target: "main".into(),
                cards: vec![card],
                source_plan: None,
            })
            .unwrap();
        INTERRUPT_AFTER_SAVE.with(|n| n.set(Some(2)));
        assert!(
            store
                .mutate(
                    "sequence",
                    &Mutation {
                        request_id: "B".into(),
                        owner: "A".into(),
                        generation: 1,
                        revision: 1,
                        action: Action::Resume {
                            new_owner: "B".into()
                        }
                    }
                )
                .is_err()
        );
        INTERRUPT_AFTER_SAVE.with(|n| n.set(None));
        let result = store
            .mutate(
                "sequence",
                &Mutation {
                    request_id: "C".into(),
                    owner: "B".into(),
                    generation: 2,
                    revision: 1,
                    action: Action::Resume {
                        new_owner: "C".into(),
                    },
                },
            )
            .unwrap();
        assert_eq!(result.outcome, Outcome::Work);
        assert_eq!(result.generation, 3);
    }

    #[test]
    fn target_rollback_after_archival_cannot_complete_or_prepare_more_work() {
        let mut released = Vec::new();
        for resume in [false, true] {
            let repo = repository();
            let base = crate::kanban::git(repo.path(), &["rev-parse", "HEAD"]).unwrap();
            let mut board = crate::kanban::CardStore::default();
            board.set_dir(Some(repo.path()));
            let card = board.add("Retain accepted work", None).unwrap();
            let common = crate::integrate::repo_identity(repo.path()).unwrap();
            let store = AssignmentStore::for_repo(&common);
            store
                .begin(&Begin {
                    id: "sequence".into(),
                    request_id: "begin".into(),
                    owner: "owner".into(),
                    checkout: repo.path().into(),
                    target: "main".into(),
                    cards: vec![card.clone()],
                    source_plan: None,
                })
                .unwrap();
            std::fs::write(repo.path().join("accepted"), "outcome").unwrap();
            crate::kanban::git(repo.path(), &["add", "accepted"]).unwrap();
            crate::kanban::git(repo.path(), &["commit", "-m", "Accept outcome"]).unwrap();
            let submitted = store
                .mutate(
                    "sequence",
                    &Mutation {
                        request_id: "submit".into(),
                        owner: "owner".into(),
                        generation: 1,
                        revision: 1,
                        action: Action::Continue { submit: true },
                    },
                )
                .unwrap();
            let queue = crate::integrate::Queue::for_repo(&common);
            crate::integrate::run_turn(
                &queue,
                &crate::integrate::Owner::this_process(),
                &crate::integrate::NoHooks,
            )
            .unwrap();
            let mut interrupted = store.get("sequence").unwrap();
            interrupted.evidence.push(queue.get(&card).unwrap());
            interrupted.current = None;
            interrupted.submission = None;
            store.save(&interrupted).unwrap();
            crate::kanban::git(repo.path(), &["update-ref", "refs/heads/main", &base]).unwrap();
            let result = store.mutate(
                "sequence",
                &Mutation {
                    request_id: "recover".into(),
                    owner: "owner".into(),
                    generation: 1,
                    revision: submitted.revision,
                    action: if resume {
                        Action::Resume {
                            new_owner: "replacement".into(),
                        }
                    } else {
                        Action::Continue { submit: false }
                    },
                },
            );
            released.push(
                result.is_ok_and(|r| r.outcome == Outcome::Complete || r.outcome == Outcome::Work),
            );
        }
        assert_eq!(
            released,
            [false, false],
            "Continue and Resume must verify archived commits against the observed target"
        );
    }

    #[test]
    fn checkpoint_retry_returns_original_result_and_rejects_changed_payload() {
        let repo = tempfile::tempdir().unwrap();
        let store = AssignmentStore::for_repo(repo.path());
        let first = store.reserve("first", repo.path(), "owner-a").unwrap();
        let request = Mutation {
            request_id: "checkpoint-1".into(),
            owner: "owner-a".into(),
            generation: first.generation,
            revision: first.revision,
            action: Action::Checkpoint {
                note: "Tests red; implement the reservation guard".into(),
            },
        };
        let saved = store.mutate("first", &request).unwrap();
        assert_eq!(saved.note, "Tests red; implement the reservation guard");
        assert_eq!(saved.revision, first.revision + 1);
        assert_eq!(store.mutate("first", &request).unwrap(), saved);
        let mut changed = request.clone();
        changed.action = Action::Checkpoint {
            note: "different".into(),
        };
        assert!(store.mutate("first", &changed).is_err());
        changed.request_id = "checkpoint-2".into();
        assert!(store.mutate("first", &changed).is_err());
        assert_eq!(store.get("first").unwrap().note, saved.note);
    }

    fn mutation(begin: &Begin, request_id: &str, at: &Snapshot, action: Action) -> Mutation {
        Mutation {
            request_id: request_id.into(),
            owner: begin.owner.clone(),
            generation: at.generation,
            revision: at.revision,
            action,
        }
    }

    fn commit_outcome(repo: &Path) {
        std::fs::write(repo.join("outcome.txt"), "accepted work").unwrap();
        crate::kanban::git(repo, &["add", "outcome.txt"]).unwrap();
        crate::kanban::git(repo, &["commit", "-m", "Implement outcome"]).unwrap();
    }

    #[test]
    fn plain_continue_after_a_cancelled_integration_does_not_resubmit() {
        let (repo, _board, store, begin, result) = begun();
        commit_outcome(repo.path());
        let submit = Action::Continue { submit: true };
        let result = store
            .mutate(&begin.id, &mutation(&begin, "submit", &result, submit))
            .unwrap();
        assert_eq!(result.outcome, Outcome::Waiting);
        let queue = crate::integrate::Queue::for_repo(
            &crate::integrate::repo_identity(repo.path()).unwrap(),
        );
        queue.cancel(&begin.cards[0]).unwrap();
        let observe = Action::Continue { submit: false };
        let result = store
            .mutate(&begin.id, &mutation(&begin, "observe", &result, observe))
            .unwrap();
        assert_eq!(result.outcome, Outcome::RecoveryRequired);
        assert!(queue.get(&begin.cards[0]).is_none(), "the cancel stands");
    }

    #[test]
    fn a_failed_continue_does_not_block_checkpoint_or_pause() {
        let (repo, _board, store, begin, result) = begun();
        crate::kanban::git(repo.path(), &["switch", "main"]).unwrap();
        let observe = Action::Continue { submit: false };
        assert!(
            store
                .mutate(&begin.id, &mutation(&begin, "observe", &result, observe))
                .is_err()
        );
        let note = Action::Checkpoint {
            note: "Switched away to inspect main".into(),
        };
        let result = store
            .mutate(&begin.id, &mutation(&begin, "note", &result, note))
            .unwrap();
        assert_eq!(result.note, "Switched away to inspect main");
        let result = store
            .mutate(
                &begin.id,
                &mutation(&begin, "pause", &result, Action::Pause),
            )
            .unwrap();
        assert!(store.get(&begin.id).unwrap().pause_requested);
        assert_eq!(result.current.as_deref(), Some(begin.cards[0].as_str()));
    }

    #[test]
    fn a_failed_continue_after_claiming_a_successor_still_requires_replay() {
        let (_repo, _board, store, begin, result) = begun();
        // Simulate a Continue that claimed a successor, then lost its response.
        let mut record = store.get(&begin.id).unwrap();
        let interrupted = mutation(
            &begin,
            "advance",
            &result,
            Action::Continue { submit: false },
        );
        record.pending = Some(Pending {
            request: interrupted.clone(),
            starting_card: Some("predecessor".into()),
        });
        store.save(&record).unwrap();
        let pause = mutation(&begin, "pause", &result, Action::Pause);
        assert!(store.mutate(&begin.id, &pause).is_err());
        assert_eq!(
            store.get(&begin.id).unwrap().pending.unwrap().request,
            interrupted
        );
    }

    #[test]
    fn blocked_assignment_can_release_its_checkout_explicitly() {
        let (repo, mut board, store, begin, result) = begun();
        board.reload();
        board.block(&begin.cards[0], "Unclear acceptance").unwrap();
        let observe = Action::Continue { submit: false };
        let result = store
            .mutate(&begin.id, &mutation(&begin, "observe", &result, observe))
            .unwrap();
        assert_eq!(result.outcome, Outcome::Blocked);
        assert!(guard_checkout(repo.path()).is_err());
        let release = mutation(&begin, "release", &result, Action::Release);
        let released = store.mutate(&begin.id, &release).unwrap();
        assert_eq!(released.outcome, Outcome::Released);
        assert_eq!(store.mutate(&begin.id, &release).unwrap(), released);
        assert!(guard_checkout(repo.path()).is_ok());
        assert!(guard_card(repo.path(), &begin.cards[0]).is_ok());
        board.reload();
        let card = board.get(&begin.cards[0]).unwrap();
        assert_eq!(
            card.state,
            crate::kanban::CardState::Blocked,
            "board untouched"
        );
        assert_eq!(
            crate::kanban::git(repo.path(), &["branch", "--show-current"]).unwrap(),
            format!("card/{}", begin.cards[0]),
            "git untouched"
        );
        let later = mutation(&begin, "later", &released, Action::Pause);
        assert!(store.mutate(&begin.id, &later).is_err());
    }

    #[test]
    fn release_is_refused_while_integration_is_in_flight() {
        let (repo, _board, store, begin, result) = begun();
        commit_outcome(repo.path());
        let submit = Action::Continue { submit: true };
        let result = store
            .mutate(&begin.id, &mutation(&begin, "submit", &result, submit))
            .unwrap();
        let release = mutation(&begin, "release", &result, Action::Release);
        assert!(store.mutate(&begin.id, &release).is_err());
        assert!(guard_checkout(repo.path()).is_err());
        let queue = crate::integrate::Queue::for_repo(
            &crate::integrate::repo_identity(repo.path()).unwrap(),
        );
        assert!(queue.get(&begin.cards[0]).is_some());
        // Another request is not wedged by the refused release.
        let pause = mutation(&begin, "pause", &result, Action::Pause);
        assert!(store.mutate(&begin.id, &pause).is_ok());
    }

    #[test]
    fn commands_from_another_version_tolerate_missing_fields() {
        let command: Command =
            serde_json::from_str(r#"{"operation":"status","id":"run"}"#).unwrap();
        assert_eq!(command.operation, "status");
        assert!(command.cards.is_empty() && command.generation.is_none());
    }

    #[test]
    fn receipts_keep_recent_replays_and_old_request_ids_cannot_reapply() {
        let repo = tempfile::tempdir().unwrap();
        let store = AssignmentStore::for_repo(repo.path());
        let reserved = store.reserve("notes", repo.path(), "owner").unwrap();
        let (mut revision, mut last) = (reserved.revision, None);
        for i in 0..40 {
            let request = Mutation {
                request_id: format!("note-{i}"),
                owner: "owner".into(),
                generation: reserved.generation,
                revision,
                action: Action::Checkpoint {
                    note: format!("note {i}"),
                },
            };
            let result = store.mutate("notes", &request).unwrap();
            revision = result.revision;
            last = Some((request, result));
        }
        let record = store.get("notes").unwrap();
        assert!(record.receipts.len() <= 16, "{}", record.receipts.len());
        let (request, result) = last.unwrap();
        assert_eq!(store.mutate("notes", &request).unwrap(), result);
        let first = Mutation {
            request_id: "note-0".into(),
            owner: "owner".into(),
            generation: reserved.generation,
            revision: reserved.revision,
            action: Action::Checkpoint {
                note: "note 0".into(),
            },
        };
        assert!(store.mutate("notes", &first).is_err());
        assert_eq!(store.get("notes").unwrap().note, "note 39");
    }

    #[test]
    fn finished_assignments_leave_the_active_set_but_keep_their_replay_and_id() {
        let (repo, mut board, store, begin, result) = begun();
        board.reload();
        board.block(&begin.cards[0], "Unclear acceptance").unwrap();
        let observe = Action::Continue { submit: false };
        let result = store
            .mutate(&begin.id, &mutation(&begin, "observe", &result, observe))
            .unwrap();
        let release = mutation(&begin, "release", &result, Action::Release);
        let released = store.mutate(&begin.id, &release).unwrap();
        let common = crate::integrate::repo_identity(repo.path()).unwrap();
        let assignments = common.join("foreman/assignments");
        assert!(!assignments.join("sequence.json").exists());
        assert_eq!(store.mutate(&begin.id, &release).unwrap(), released);
        // A damaged finished record cannot block unrelated work.
        std::fs::write(assignments.join("finished/sequence.json"), "{").unwrap();
        assert!(guard_checkout(repo.path()).is_ok());
        assert!(guard_card(repo.path(), "other").is_ok());
        let card = board.add("Another outcome", None).unwrap();
        crate::kanban::git(repo.path(), &["switch", "main"]).unwrap();
        let reused = Begin {
            request_id: "begin-again".into(),
            cards: vec![card],
            ..begin.clone()
        };
        assert!(
            store.begin(&reused).is_err(),
            "an assignment id is never reused"
        );
    }

    #[test]
    fn legacy_card_guards_touch_nothing_in_a_repository_without_assignments() {
        let repo = repository();
        assert!(guard_card(repo.path(), "card").unwrap().is_none());
        let common = crate::integrate::repo_identity(repo.path()).unwrap();
        assert!(!common.join("foreman").exists());
    }

    #[test]
    fn file_discovery_matches_git_for_roots_subdirectories_and_linked_worktrees() {
        let repo = repository();
        std::fs::create_dir(repo.path().join("sub")).unwrap();
        let linked = tempfile::tempdir().unwrap();
        let tree = linked.path().join("tree");
        crate::kanban::git(
            repo.path(),
            &["worktree", "add", "-q", "--detach", &tree.to_string_lossy()],
        )
        .unwrap();
        for dir in [repo.path().to_path_buf(), repo.path().join("sub"), tree] {
            let (common, root) = discover_checkout(&dir).expect("discovered");
            assert_eq!(common, crate::integrate::repo_identity(&dir).unwrap());
            let git_root = crate::kanban::git(&dir, &["rev-parse", "--show-toplevel"]).unwrap();
            assert!(
                crate::kanban::same_path(&root.to_string_lossy(), &git_root),
                "{root:?} vs {git_root}"
            );
        }
        let plain = tempfile::tempdir().unwrap();
        assert!(discover_checkout(plain.path()).is_none());
    }

    #[test]
    fn a_submit_whose_response_was_lost_after_queueing_is_never_queued_twice() {
        let (repo, _board, store, begin, result) = begun();
        commit_outcome(repo.path());
        let submit = mutation(&begin, "submit", &result, Action::Continue { submit: true });
        let answered = store.mutate(&begin.id, &submit).unwrap();
        let queue = crate::integrate::Queue::for_repo(
            &crate::integrate::repo_identity(repo.path()).unwrap(),
        );
        let queued = queue.get(&begin.cards[0]).unwrap();
        // Crash after the queue write, before the receipt: roll the record
        // back to the durable state such a crash leaves behind.
        let mut record = store.get(&begin.id).unwrap();
        record.receipts.retain(|r| r.request != submit);
        record.revision = result.revision;
        record.outcome = Outcome::Work;
        record.pending = Some(Pending {
            request: submit.clone(),
            starting_card: record.current.clone(),
        });
        store.save(&record).unwrap();
        let replayed = store.mutate(&begin.id, &submit).unwrap();
        assert_eq!(replayed, answered);
        let retry = mutation(
            &begin,
            "submit-again",
            &replayed,
            Action::Continue { submit: true },
        );
        assert_eq!(
            store.mutate(&begin.id, &retry).unwrap().outcome,
            Outcome::Waiting
        );
        let after = queue.get(&begin.cards[0]).unwrap();
        assert_eq!((after.seq, &after.commit), (queued.seq, &queued.commit));
        assert_eq!(queue.requests().len(), 1);
    }

    #[test]
    fn every_command_result_discloses_compaction_recovery_for_its_provider() {
        let repo = repository();
        let mut board = crate::kanban::CardStore::default();
        board.set_dir(Some(repo.path()));
        let card = board.add("Outcome", None).unwrap();
        let owner = OwnerIdentity {
            instance: "instance".into(),
            project: "p1".into(),
            terminal: "t1".into(),
            provider: "claude".into(),
            session: "session".into(),
            process: crate::proc::ProviderProcess { pid: 7, started: 9 },
        };
        let command = |operation: &str| Command {
            operation: operation.into(),
            id: "run".into(),
            request_id: format!("{operation}-1"),
            provider: "claude".into(),
            session: "session".into(),
            caller_pid: 1,
            cards: vec![card.clone()],
            target: "main".into(),
            ..Default::default()
        };
        let begun = command("begin").execute(repo.path(), &owner).unwrap();
        let status = command("status").execute(repo.path(), &owner).unwrap();
        for result in [&begun, &status] {
            let recovery = result.recovery.as_deref().unwrap_or_default();
            assert!(recovery.contains("assignment status"), "{result:?}");
        }
        // Reported per response, never persisted into replayable receipts.
        let stored =
            AssignmentStore::for_repo(&crate::integrate::repo_identity(repo.path()).unwrap())
                .get("run")
                .unwrap();
        assert!(stored.begin_result.unwrap().recovery.is_none());
    }

    #[test]
    fn a_target_that_moves_while_waiting_returns_the_card_without_advancing() {
        let repo = repository();
        let mut board = crate::kanban::CardStore::default();
        board.set_dir(Some(repo.path()));
        let first = board.add("First outcome", None).unwrap();
        let second = board.add("Second outcome", None).unwrap();
        let common = crate::integrate::repo_identity(repo.path()).unwrap();
        let store = AssignmentStore::for_repo(&common);
        let begin = Begin {
            id: "sequence".into(),
            request_id: "begin".into(),
            owner: "owner".into(),
            checkout: repo.path().into(),
            target: "main".into(),
            source_plan: None,
            cards: vec![first.clone(), second.clone()],
        };
        let result = store.begin(&begin).unwrap();
        let current = result.current.clone().unwrap();
        commit_outcome(repo.path());
        let submit = Action::Continue { submit: true };
        let result = store
            .mutate(&begin.id, &mutation(&begin, "submit", &result, submit))
            .unwrap();
        assert_eq!(result.outcome, Outcome::Waiting);
        // Someone else lands on main while the card waits in the queue.
        let git = |args: &[&str]| crate::kanban::git(repo.path(), args).unwrap();
        let tree = git(&["rev-parse", "main^{tree}"]);
        let moved = git(&["commit-tree", &tree, "-p", "main", "-m", "External change"]);
        git(&["update-ref", "refs/heads/main", &moved]);
        let queue = crate::integrate::Queue::for_repo(&common);
        crate::integrate::run_turn(
            &queue,
            &crate::integrate::Owner::this_process(),
            &crate::integrate::NoHooks,
        )
        .unwrap();
        let observe = Action::Continue { submit: false };
        let result = store
            .mutate(&begin.id, &mutation(&begin, "observe", &result, observe))
            .unwrap();
        assert_eq!(result.outcome, Outcome::RecoveryRequired);
        assert_eq!(result.current.as_deref(), Some(current.as_str()));
        board.reload();
        let other = if current == first { &second } else { &first };
        assert_eq!(
            board.get(other).unwrap().state,
            crate::kanban::CardState::Backlog
        );
        assert_eq!(
            git(&["rev-parse", "main"]),
            moved,
            "the target keeps the external change"
        );
        // The same agent repairs onto the moved target and resubmits.
        git(&["rebase", "-q", "main"]);
        let submit = Action::Continue { submit: true };
        let result = store
            .mutate(&begin.id, &mutation(&begin, "resubmit", &result, submit))
            .unwrap();
        assert_eq!(result.outcome, Outcome::Waiting);
        crate::integrate::run_turn(
            &queue,
            &crate::integrate::Owner::this_process(),
            &crate::integrate::NoHooks,
        )
        .unwrap();
        let advance = Action::Continue { submit: false };
        let result = store
            .mutate(&begin.id, &mutation(&begin, "advance", &result, advance))
            .unwrap();
        assert_eq!(result.outcome, Outcome::Work);
        assert_eq!(result.current.as_deref(), Some(other.as_str()));
    }
}
