//! Pure card domain for the per-project kanban board: file-per-card storage
//! under `.foreman/tasks/`, single-writer transitions, and derived orphan
//! state (a claim whose Session is gone). GUI-free and fully unit-testable —
//! `wm.rs` (Task 3) owns the rendering and dispatch wiring; `control.rs`
//! (Task 2) owns the wire verb and CLI. See
//! `docs/superpowers/specs/2026-08-28-kanban-board-design.md` for the full
//! design and the transition/verdict tables this module implements.

/// Card file schema version. Bump only alongside a documented migration.
pub const CARD_V: u32 = 1;

/// How often [`CardStore::maybe_reload`] re-checks the on-disk fingerprint —
/// the branch-switch/pull staleness poll from the spec's Reconciliation
/// section, not a live-update mechanism.
pub const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardState {
    Backlog,
    InProgress,
    Blocked,
    Done,
}

/// The card↔Session link recorded at dispatch or `start`. Dead claims are
/// derived (see [`claim_is_dead`]), never stored.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Claim {
    pub terminal: String,
    pub run: String,
    pub agent: Option<String>,
    pub at: String,
}

/// One unit of work-in-flight on a project's board — a file in
/// `.foreman/tasks/` owned by the app.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Card {
    pub v: u32,
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub state: CardState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<Claim>,
    pub created: String,
    pub updated: String,
}

impl Card {
    pub fn new(id: String, title: String, body: Option<String>, now: String) -> Self {
        Card {
            v: CARD_V,
            id,
            title,
            body,
            state: CardState::Backlog,
            blocked_reason: None,
            claim: None,
            created: now.clone(),
            updated: now,
        }
    }
}

/// A Session's liveness, as observed by the wm tick that drives
/// [`is_orphaned`]. `Missing` covers both "no such terminal" and "terminal id
/// unknown to the caller" — the two are indistinguishable from a card's view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermState {
    Missing,
    Running,
    Exited,
}

/// RFC3339 timestamp, seconds precision, always UTC (`Z` suffix) — the shape
/// every card field and test fixture in this module uses.
fn now_stamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// An app-instance nonce: unique per foreman launch, not cryptographically
/// strong. Used to tell "my claim" from "some other/earlier launch's claim"
/// without a PID (PIDs get reused across restarts).
pub fn run_nonce() -> &'static str {
    use std::sync::OnceLock;
    static NONCE: OnceLock<String> = OnceLock::new();
    NONCE.get_or_init(|| {
        use sha2::{Digest, Sha256};
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = std::process::id();
        let mut hasher = Sha256::new();
        hasher.update(nanos.to_le_bytes());
        hasher.update(pid.to_le_bytes());
        let digest = hasher.finalize();
        let hex = digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        hex.chars().take(32).collect()
    })
}

/// Base36 alphabet for generated ids: digits then lowercase letters, so ids
/// stay readable and shell/filename-safe with no escaping.
const BASE36: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// Six-char base36 id, regenerated on collision. Single writer per spec, so
/// regenerate-on-hit is the whole collision story — no locking needed.
fn gen_id(existing: &std::collections::HashSet<String>) -> String {
    use sha2::{Digest, Sha256};
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    loop {
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let mut hasher = Sha256::new();
        hasher.update(run_nonce().as_bytes());
        hasher.update(n.to_le_bytes());
        hasher.update(nanos.to_le_bytes());
        let digest = hasher.finalize();
        let id: String = digest
            .iter()
            .take(6)
            .map(|b| BASE36[(*b as usize) % BASE36.len()] as char)
            .collect();
        if !existing.contains(&id) {
            return id;
        }
    }
}

/// A claim is dead when it points at a Session that can no longer answer:
/// a different app launch's run nonce (stale after a restart, PIDs get
/// reused), or the claimed terminal is gone / has exited. `None` (no claim
/// at all) counts as dead too — callers pass the state of the claim's own
/// terminal in `term`.
pub fn claim_is_dead(claim: Option<&Claim>, current_run: &str, term: TermState) -> bool {
    match claim {
        None => true,
        Some(c) => c.run != current_run || term != TermState::Running,
    }
}

/// Only an InProgress card can be orphaned — Backlog/Blocked/Done have no
/// live claim to lose. The terminal's liveness comes from `states`;
/// `Missing` when the card's claimed terminal isn't in the map at all.
pub fn is_orphaned(
    card: &Card,
    current_run: &str,
    states: &std::collections::HashMap<String, TermState>,
) -> bool {
    if card.state != CardState::InProgress {
        return false;
    }
    let term = card
        .claim
        .as_ref()
        .and_then(|c| states.get(&c.terminal).copied())
        .unwrap_or(TermState::Missing);
    claim_is_dead(card.claim.as_ref(), current_run, term)
}

/// (file name, mtime, len) for one card file — cheap enough to stat every
/// file in the tasks dir without parsing JSON, used to detect external
/// changes (branch switch, `git pull`, hand-edited file) between polls.
type Fingerprint = Vec<(String, std::time::SystemTime, u64)>;

fn sort_cards(cards: &mut [Card]) {
    cards.sort_by(|a, b| (&a.created, &a.id).cmp(&(&b.created, &b.id)));
}

fn fingerprint_of(dir: Option<&std::path::Path>) -> Fingerprint {
    let Some(dir) = dir else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut fp: Fingerprint = entries
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            Some((
                e.file_name().to_string_lossy().into_owned(),
                mtime,
                meta.len(),
            ))
        })
        .collect();
    fp.sort();
    fp
}

/// File-per-card store for one project's `.foreman/tasks/` directory. Files
/// are authoritative over memory: every mutation re-reads its target file
/// first (so a concurrent hand-edit or another foreman instance's write is
/// never silently clobbered), writes atomically (temp file + rename), then
/// updates `cards` + `fingerprint` in memory.
#[derive(Debug, Default)]
pub struct CardStore {
    dir: Option<std::path::PathBuf>,
    cards: Vec<Card>,
    fingerprint: Fingerprint,
    last_poll: Option<std::time::Instant>,
    last_shown: Option<std::time::Instant>,
    orphans: std::collections::HashSet<String>,
}

impl CardStore {
    /// Point the store at `<project_cwd>/.foreman/tasks` (or clear it).
    /// Idempotent — safe to call every tick; only reloads when the resolved
    /// directory actually changes (e.g. focused project switched).
    pub fn set_dir(&mut self, project_cwd: Option<&std::path::Path>) {
        let new_dir = project_cwd.map(|p| p.join(".foreman").join("tasks"));
        if new_dir != self.dir {
            self.dir = new_dir;
            self.reload();
        }
    }

    /// Read every `*.json` in the tasks dir into `cards`. A missing dir
    /// (never created — no card added yet) is an empty store, not an error.
    /// An unparseable file is skipped with an `eprintln!`, never a panic —
    /// one corrupt card must not take down the whole board.
    pub fn reload(&mut self) {
        let Some(dir) = self.dir.clone() else {
            self.cards.clear();
            self.fingerprint.clear();
            return;
        };
        let mut cards = Vec::new();
        match std::fs::read_dir(&dir) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("json") {
                        continue;
                    }
                    match std::fs::read_to_string(&path) {
                        Ok(text) => match serde_json::from_str::<Card>(&text) {
                            Ok(card) => cards.push(card),
                            Err(e) => eprintln!(
                                "kanban: skipping unparseable card {}: {e}",
                                path.display()
                            ),
                        },
                        Err(e) => {
                            eprintln!("kanban: cannot read card {}: {e}", path.display())
                        }
                    }
                }
            }
            Err(_) => {
                // Missing dir = empty store; it's only created on first `add`.
            }
        }
        sort_cards(&mut cards);
        self.cards = cards;
        self.fingerprint = fingerprint_of(Some(&dir));
    }

    /// No-op unless [`POLL_INTERVAL`] has elapsed since the last poll; then
    /// reload only if the on-disk fingerprint actually changed. This is the
    /// branch-switch/`git pull` staleness poll — not a live-update mechanism.
    pub fn maybe_reload(&mut self, now: std::time::Instant) {
        if let Some(last) = self.last_poll {
            if now.duration_since(last) < POLL_INTERVAL {
                return;
            }
        }
        self.last_poll = Some(now);
        if fingerprint_of(self.dir.as_deref()) != self.fingerprint {
            self.reload();
        }
    }

    pub fn cards(&self) -> &[Card] {
        &self.cards
    }

    pub fn get(&self, id: &str) -> Option<&Card> {
        self.cards.iter().find(|c| c.id == id)
    }

    pub fn orphans(&self) -> &std::collections::HashSet<String> {
        &self.orphans
    }

    pub fn set_orphans(&mut self, o: std::collections::HashSet<String>) {
        self.orphans = o;
    }

    /// Stamped by the board view each rendered frame; see [`Self::shown_recently`].
    pub fn mark_shown(&mut self, now: std::time::Instant) {
        self.last_shown = Some(now);
    }

    /// True within 1s of the last [`Self::mark_shown`] stamp — gates the
    /// staleness poll to a board that is actually on screen.
    pub fn shown_recently(&self, now: std::time::Instant) -> bool {
        self.last_shown
            .is_some_and(|t| now.duration_since(t) < std::time::Duration::from_secs(1))
    }

    fn dir_or_err(&self) -> Result<&std::path::Path, String> {
        self.dir
            .as_deref()
            .ok_or_else(|| "no project selected".to_string())
    }

    /// Read one card's file straight from disk — the "files are authoritative
    /// over memory" re-read every mutation performs before touching a card.
    fn read_one(&self, id: &str) -> Result<Card, String> {
        let dir = self.dir_or_err()?;
        let path = dir.join(format!("{id}.json"));
        let text = std::fs::read_to_string(&path).map_err(|_| format!("no such card: {id}"))?;
        serde_json::from_str(&text).map_err(|e| format!("card {id} is corrupt: {e}"))
    }

    /// Atomic write: temp file in the same dir, then rename — a reader (or a
    /// crash) never observes a half-written card file.
    fn write_card(&self, dir: &std::path::Path, card: &Card) -> Result<(), String> {
        let path = dir.join(format!("{}.json", card.id));
        let tmp = dir.join(format!("{}.json.tmp", card.id));
        let json = serde_json::to_string_pretty(card).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, json).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path)
            .map_err(|e| format!("cannot finalize {}: {e}", path.display()))?;
        Ok(())
    }

    /// Update the in-memory mirror after a successful write: replace (or
    /// insert) the card, re-sort, and refresh the fingerprint so a later
    /// `maybe_reload` doesn't mistake our own write for an external change.
    fn replace_in_memory(&mut self, card: Card) {
        if let Some(existing) = self.cards.iter_mut().find(|c| c.id == card.id) {
            *existing = card;
        } else {
            self.cards.push(card);
        }
        sort_cards(&mut self.cards);
        self.fingerprint = fingerprint_of(self.dir.as_deref());
    }

    /// Reject an empty/whitespace title; generate a fresh id against the
    /// current on-disk set (a `reload` first, so two concurrent `add`s from
    /// different processes don't collide); create the tasks dir on first use.
    pub fn add(&mut self, title: &str, body: Option<&str>) -> Result<String, String> {
        let title = title.trim();
        if title.is_empty() {
            return Err("title cannot be empty".into());
        }
        let dir = self.dir.clone().ok_or("no project selected")?;
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        self.reload();
        let existing: std::collections::HashSet<String> =
            self.cards.iter().map(|c| c.id.clone()).collect();
        let id = gen_id(&existing);
        let card = Card::new(
            id.clone(),
            title.to_string(),
            body.map(str::to_string),
            now_stamp(),
        );
        self.write_card(&dir, &card)?;
        self.replace_in_memory(card);
        Ok(id)
    }

    /// Shared claim transition for `start` (self-service pickup, no agent)
    /// and `claim_for_dispatch` (records the dispatched agent). Allowed from
    /// Backlog, Blocked, or InProgress-with-a-dead-claim (seize); rejected
    /// from InProgress-with-a-live-claim (the two-agents-one-card guard) and
    /// from Done.
    fn claim_common(
        &mut self,
        id: &str,
        terminal: &str,
        agent: Option<&str>,
        current_run: &str,
        term: TermState,
    ) -> Result<(), String> {
        let dir = self.dir_or_err()?.to_path_buf();
        let mut card = self.read_one(id)?;
        let allowed = match card.state {
            CardState::Backlog | CardState::Blocked => true,
            CardState::InProgress => claim_is_dead(card.claim.as_ref(), current_run, term),
            CardState::Done => false,
        };
        if !allowed {
            return Err(format!(
                "card {id} cannot be claimed from its current state ({:?})",
                card.state
            ));
        }
        card.claim = Some(Claim {
            terminal: terminal.to_string(),
            run: current_run.to_string(),
            agent: agent.map(str::to_string),
            at: now_stamp(),
        });
        card.state = CardState::InProgress;
        card.blocked_reason = None;
        card.updated = now_stamp();
        self.write_card(&dir, &card)?;
        self.replace_in_memory(card);
        Ok(())
    }

    /// Self-service pickup: an agent claims its own card, no dispatch involved.
    pub fn start(
        &mut self,
        id: &str,
        terminal: &str,
        current_run: &str,
        term: TermState,
    ) -> Result<(), String> {
        self.claim_common(id, terminal, None, current_run, term)
    }

    /// Claim recorded by the board's dispatch drain after a successful spawn
    /// — a card-spawned agent never runs `start` itself.
    pub fn claim_for_dispatch(
        &mut self,
        id: &str,
        terminal: &str,
        agent: &str,
        current_run: &str,
        term: TermState,
    ) -> Result<(), String> {
        self.claim_common(id, terminal, Some(agent), current_run, term)
    }

    /// InProgress -> Done only; clears the claim. Missing card = error, never
    /// created (close-out never resurrects a card). No claimant check — same
    /// trust model as chat (guardrail, not a security boundary).
    pub fn done(&mut self, id: &str) -> Result<(), String> {
        let dir = self.dir_or_err()?.to_path_buf();
        let mut card = self.read_one(id)?;
        if card.state != CardState::InProgress {
            return Err(format!("card {id} is not in progress"));
        }
        card.state = CardState::Done;
        card.claim = None;
        card.updated = now_stamp();
        self.write_card(&dir, &card)?;
        self.replace_in_memory(card);
        Ok(())
    }

    /// InProgress -> Blocked only; clears the claim, records `reason`. A
    /// nonempty reason is mandatory — a Blocked column without reasons costs
    /// the human an investigation per card.
    pub fn block(&mut self, id: &str, reason: &str) -> Result<(), String> {
        if reason.trim().is_empty() {
            return Err("block reason cannot be empty".into());
        }
        let dir = self.dir_or_err()?.to_path_buf();
        let mut card = self.read_one(id)?;
        if card.state != CardState::InProgress {
            return Err(format!("card {id} is not in progress"));
        }
        card.state = CardState::Blocked;
        card.claim = None;
        card.blocked_reason = Some(reason.to_string());
        card.updated = now_stamp();
        self.write_card(&dir, &card)?;
        self.replace_in_memory(card);
        Ok(())
    }

    /// InProgress or Blocked -> Backlog; clears claim + reason. Board-only
    /// recovery action; deliberately NOT a wire verb (the spec's verb table
    /// is closed).
    pub fn release(&mut self, id: &str) -> Result<(), String> {
        let dir = self.dir_or_err()?.to_path_buf();
        let mut card = self.read_one(id)?;
        if !matches!(card.state, CardState::InProgress | CardState::Blocked) {
            return Err(format!(
                "card {id} cannot be released from its current state ({:?})",
                card.state
            ));
        }
        card.state = CardState::Backlog;
        card.claim = None;
        card.blocked_reason = None;
        card.updated = now_stamp();
        self.write_card(&dir, &card)?;
        self.replace_in_memory(card);
        Ok(())
    }

    /// Delete the card's file, from any state; error if it doesn't exist.
    pub fn rm(&mut self, id: &str) -> Result<(), String> {
        let dir = self.dir_or_err()?.to_path_buf();
        let path = dir.join(format!("{id}.json"));
        std::fs::remove_file(&path).map_err(|_| format!("no such card: {id}"))?;
        self.cards.retain(|c| c.id != id);
        self.fingerprint = fingerprint_of(Some(&dir));
        Ok(())
    }
}

/// How the dispatch prompt addresses the foreman CLI. The installed exe is
/// on the user PATH (install.ps1 adds %LOCALAPPDATA%\Programs\foreman), so
/// it renders plain `foreman`; a dev/debug build renders the FOREMAN_EXE
/// env-var form that works without PATH.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseoutStyle {
    Path,
    EnvVar,
}

/// `Path` iff the running exe sits under `%LOCALAPPDATA%\Programs\foreman`
/// (case-insensitive parent-dir compare); any error or non-match is
/// `EnvVar` — the safe default that works in a dev fleet with no PATH entry.
pub fn closeout_style() -> CloseoutStyle {
    let Ok(exe) = std::env::current_exe() else {
        return CloseoutStyle::EnvVar;
    };
    let Some(parent) = exe.parent() else {
        return CloseoutStyle::EnvVar;
    };
    let Some(local_appdata) = std::env::var_os("LOCALAPPDATA") else {
        return CloseoutStyle::EnvVar;
    };
    let installed = std::path::PathBuf::from(local_appdata)
        .join("Programs")
        .join("foreman");
    if parent
        .to_string_lossy()
        .eq_ignore_ascii_case(&installed.to_string_lossy())
    {
        CloseoutStyle::Path
    } else {
        CloseoutStyle::EnvVar
    }
}

/// Renders the spec's dispatch-prompt template verbatim: fixed text, card
/// fields interpolated, nothing else. The close-out block is the only part
/// that varies, by [`CloseoutStyle`] (installed-on-PATH vs. dev-fleet).
pub fn dispatch_prompt(card: &Card, style: CloseoutStyle) -> String {
    let header = format!(
        "You are a worker Session dispatched from card {id} on this project's board.\n\
         \n\
         # Task: {title}\n\
         \n\
         {body}\n\
         \n",
        id = card.id,
        title = card.title,
        body = card.body.as_deref().unwrap_or(""),
    );
    let closeout = match style {
        CloseoutStyle::Path => format!(
            "# Close-out (required)\n\
             When the work is complete, run:    foreman kanban done {id}\n\
             If you are stuck and need a human: foreman kanban block {id} --reason \"<one line>\"\n\
             Do not end the session without running one of these.",
            id = card.id,
        ),
        CloseoutStyle::EnvVar => format!(
            "# Close-out (required)\n\
             When the work is complete, run:    & $env:FOREMAN_EXE kanban done {id}\n\
             If you are stuck and need a human: & $env:FOREMAN_EXE kanban block {id} --reason \"<one line>\"\n\
             (bash: write \"$FOREMAN_EXE\" in place of & $env:FOREMAN_EXE)\n\
             Do not end the session without running one of these.",
            id = card.id,
        ),
    };
    header + &closeout
}

/// A card plus the derived `orphaned` flag (never stored in the card file
/// itself — see [`is_orphaned`]) for the agent- and human-facing list views.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CardLine {
    #[serde(flatten)]
    pub card: Card,
    pub orphaned: bool,
}

impl CardLine {
    /// One JSON object per line — the `list --json` wire format.
    pub fn json_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// One aligned human line: id, state, title, then a context tail —
    /// `[terminal agent]` while claimed, `(reason)` when blocked, an
    /// `ORPHANED` marker when the claim is dead.
    pub fn human_line(&self) -> String {
        let state = match self.card.state {
            CardState::Backlog => "backlog",
            CardState::InProgress => "in_progress",
            CardState::Blocked => "blocked",
            CardState::Done => "done",
        };
        let mut tail = Vec::new();
        if let Some(claim) = &self.card.claim {
            match &claim.agent {
                Some(agent) => tail.push(format!("[{} {agent}]", claim.terminal)),
                None => tail.push(format!("[{}]", claim.terminal)),
            }
        }
        if let Some(reason) = &self.card.blocked_reason {
            tail.push(format!("({reason})"));
        }
        if self.orphaned {
            tail.push("ORPHANED".to_string());
        }
        let mut line = format!("{}  {state}  {}", self.card.id, self.card.title);
        if !tail.is_empty() {
            line.push_str("  ");
            line.push_str(&tail.join(" "));
        }
        line
    }
}

/// What `foreman kanban wait` is watching for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitTarget {
    Id(String),
    Any,
}

/// Pure verdict function the CLI wait loop (Task 2) drives on every poll.
/// `Id`: Done -> exit 0; Blocked/orphaned/missing -> exit 1 (something needs
/// a human); Backlog or live InProgress -> keep waiting.
/// `Any`: every live (non-orphaned) InProgress card is added to `watched` on
/// sight, then each watched id is checked the same way — a card sitting in
/// Backlog is never watched and so never triggers.
pub fn wait_verdict(
    target: &WaitTarget,
    watched: &mut std::collections::HashSet<String>,
    cards: &[CardLine],
) -> Option<i32> {
    match target {
        WaitTarget::Id(id) => match cards.iter().find(|c| &c.card.id == id) {
            None => Some(1), // removed under the waiter
            Some(c) => match c.card.state {
                CardState::Done => Some(0),
                CardState::Blocked => Some(1),
                _ if c.orphaned => Some(1),
                CardState::Backlog | CardState::InProgress => None,
            },
        },
        WaitTarget::Any => {
            for c in cards {
                if c.card.state == CardState::InProgress && !c.orphaned {
                    watched.insert(c.card.id.clone());
                }
            }
            for id in watched.iter() {
                match cards.iter().find(|c| &c.card.id == id) {
                    None => return Some(1),
                    Some(c) => match c.card.state {
                        CardState::Done => return Some(0),
                        CardState::Blocked => return Some(1),
                        _ if c.orphaned => return Some(1),
                        _ => {}
                    },
                }
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_is_dead_covers_the_full_derivation_table() {
        let live = Claim {
            terminal: "t4".into(),
            run: "R".into(),
            agent: None,
            at: String::new(),
        };
        // (claim, current_run, term_state) -> dead?
        let cases = [
            (None, "R", TermState::Running, true),            // no claim
            (Some(&live), "OTHER", TermState::Running, true), // stale run nonce
            (Some(&live), "R", TermState::Missing, true),     // terminal gone
            (Some(&live), "R", TermState::Exited, true),      // terminal exited
            (Some(&live), "R", TermState::Running, false),    // alive
        ];
        for (claim, run, term, want) in cases {
            assert_eq!(
                claim_is_dead(claim, run, term),
                want,
                "{claim:?} {run} {term:?}"
            );
        }
    }

    #[test]
    fn is_orphaned_only_fires_on_in_progress_cards() {
        let dead_claim = Claim {
            terminal: "t4".into(),
            run: "STALE".into(),
            agent: None,
            at: String::new(),
        };
        let states = std::collections::HashMap::new(); // t4 absent = Missing
        let mut card = Card::new("a1".into(), "t".into(), None, "2026-08-28T00:00:00Z".into());
        card.claim = Some(dead_claim.clone());

        for state in [CardState::Backlog, CardState::Blocked, CardState::Done] {
            card.state = state;
            assert!(
                !is_orphaned(&card, "CURRENT", &states),
                "{state:?} must never be orphaned"
            );
        }

        card.state = CardState::InProgress;
        assert!(is_orphaned(&card, "CURRENT", &states));
    }

    fn store_at(dir: &std::path::Path) -> CardStore {
        let mut s = CardStore::default();
        s.set_dir(Some(dir));
        s
    }

    #[test]
    fn add_then_reload_roundtrips_a_card_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_at(tmp.path());
        let id = store.add("Fix resize flicker", None).unwrap();

        let mut store2 = store_at(tmp.path());
        store2.reload();
        let card = store2.get(&id).unwrap();
        assert_eq!(card.title, "Fix resize flicker");
        assert_eq!(card.state, CardState::Backlog);
    }

    #[test]
    fn full_transition_table_is_enforced() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_at(tmp.path());
        let run = run_nonce();

        let id1 = store.add("card one", None).unwrap();
        store.start(&id1, "t1", run, TermState::Running).unwrap();
        assert_eq!(store.get(&id1).unwrap().state, CardState::InProgress);
        // live claim: a second start is rejected (two-agents-one-card guard)
        assert!(store.start(&id1, "t2", run, TermState::Running).is_err());
        store.done(&id1).unwrap();
        assert_eq!(store.get(&id1).unwrap().state, CardState::Done);
        assert!(store.get(&id1).unwrap().claim.is_none());
        assert!(store.done(&id1).is_err()); // Done is terminal

        let id2 = store.add("card two", None).unwrap();
        store.start(&id2, "t1", run, TermState::Running).unwrap();
        store.block(&id2, "waiting on design").unwrap();
        assert_eq!(store.get(&id2).unwrap().state, CardState::Blocked);
        assert_eq!(
            store.get(&id2).unwrap().blocked_reason.as_deref(),
            Some("waiting on design")
        );
        assert!(store.get(&id2).unwrap().claim.is_none());
        assert!(store.done(&id2).is_err()); // Blocked -> Done is not a legal edge
        store.start(&id2, "t3", run, TermState::Running).unwrap(); // re-claim from Blocked
        assert_eq!(store.get(&id2).unwrap().state, CardState::InProgress);
        assert!(store.get(&id2).unwrap().blocked_reason.is_none());
        store.release(&id2).unwrap();
        assert_eq!(store.get(&id2).unwrap().state, CardState::Backlog);
        assert!(store.get(&id2).unwrap().claim.is_none());
        assert!(store.done(&id2).is_err()); // Backlog -> Done is not a legal edge
        store.rm(&id2).unwrap();
        assert!(store.get(&id2).is_none());
    }

    #[test]
    fn block_demands_a_nonempty_reason() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_at(tmp.path());
        let run = run_nonce();
        let id = store.add("card", None).unwrap();
        store.start(&id, "t1", run, TermState::Running).unwrap();
        assert!(store.block(&id, "").is_err());
        assert!(store.block(&id, "   ").is_err());
        assert_eq!(store.get(&id).unwrap().state, CardState::InProgress);
    }

    #[test]
    fn closeout_on_a_missing_card_errors_and_creates_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_at(tmp.path());
        assert!(store.done("nope00").is_err());
        assert!(store.block("nope00", "reason").is_err());
        assert!(store.rm("nope00").is_err());
        // no card ever created means the tasks dir was never even made
        assert!(!tmp.path().join(".foreman").exists());
    }

    #[test]
    fn start_seizes_an_orphaned_claim_but_rejects_a_live_one() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_at(tmp.path());
        let run = run_nonce();

        let id_live = store.add("live", None).unwrap();
        store
            .start(&id_live, "t1", run, TermState::Running)
            .unwrap();
        assert!(
            store
                .start(&id_live, "t2", run, TermState::Running)
                .is_err()
        );

        let id_stale = store.add("stale", None).unwrap();
        store
            .start(&id_stale, "t1", "OLD_RUN", TermState::Running)
            .unwrap();
        // OLD_RUN no longer matches this launch's nonce: the claim is dead, seize succeeds
        store
            .start(&id_stale, "t2", run, TermState::Running)
            .unwrap();
        assert_eq!(
            store
                .get(&id_stale)
                .unwrap()
                .claim
                .as_ref()
                .unwrap()
                .terminal,
            "t2"
        );
    }

    #[test]
    fn maybe_reload_picks_up_external_file_changes_after_the_interval() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_at(tmp.path());
        let id = store.add("card", None).unwrap();
        assert_eq!(store.cards().len(), 1);

        let dir = tmp.path().join(".foreman").join("tasks");
        let extra = Card::new("zz9999".into(), "external".into(), None, now_stamp());
        std::fs::write(
            dir.join("zz9999.json"),
            serde_json::to_string(&extra).unwrap(),
        )
        .unwrap();

        let t0 = std::time::Instant::now();
        store.last_poll = Some(t0);

        // Not enough time elapsed since the last poll: no reload yet.
        store.maybe_reload(t0 + std::time::Duration::from_millis(1));
        assert_eq!(store.cards().len(), 1);

        // Past POLL_INTERVAL: picks up the external file.
        let t1 = t0 + POLL_INTERVAL + std::time::Duration::from_millis(1);
        store.maybe_reload(t1);
        assert_eq!(store.cards().len(), 2);
        assert!(store.get("zz9999").is_some());

        // A deletion shows up once the interval elapses again.
        std::fs::remove_file(dir.join(format!("{id}.json"))).unwrap();
        let t2 = t1 + POLL_INTERVAL + std::time::Duration::from_millis(1);
        store.maybe_reload(t2);
        assert_eq!(store.cards().len(), 1);
        assert!(store.get(&id).is_none());
    }

    #[test]
    fn updated_stamp_moves_on_every_transition() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_at(tmp.path());
        let run = run_nonce();
        let id = store.add("card", None).unwrap();
        let created = store.get(&id).unwrap().created.clone();
        let after_add = store.get(&id).unwrap().updated.clone();

        // Seconds-precision timestamps: cross at least one second boundary
        // between transitions instead of sleeping-and-hoping on sub-second luck.
        std::thread::sleep(std::time::Duration::from_millis(1050));
        store.start(&id, "t1", run, TermState::Running).unwrap();
        let after_start = store.get(&id).unwrap().updated.clone();
        assert_eq!(store.get(&id).unwrap().created, created);
        assert_ne!(after_start, after_add);

        std::thread::sleep(std::time::Duration::from_millis(1050));
        store.done(&id).unwrap();
        assert_eq!(store.get(&id).unwrap().created, created);
        assert_ne!(store.get(&id).unwrap().updated, after_start);
    }

    #[test]
    fn card_json_matches_spec_shape_and_omits_empty_options() {
        let c = Card::new(
            "a3f8k2".into(),
            "Fix resize flicker".into(),
            None,
            "2026-08-28T13:55:00Z".into(),
        );
        let s = serde_json::to_string(&c).unwrap();
        assert!(s.contains(r#""v":1"#));
        assert!(s.contains(r#""state":"backlog""#));
        assert!(!s.contains("body"));
        assert!(!s.contains("blocked_reason"));
        assert!(!s.contains("claim"));
        let back: Card = serde_json::from_str(&s).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn card_parse_tolerates_unknown_fields_and_full_claim() {
        let j = r#"{"v":1,"id":"a3f8k2","title":"t","state":"in_progress",
            "claim":{"terminal":"t4","run":"r1","agent":"claude","at":"2026-08-28T14:03:00Z"},
            "created":"2026-08-28T13:55:00Z","updated":"2026-08-28T14:03:00Z","future_field":true}"#;
        let c: Card = serde_json::from_str(j).unwrap();
        assert_eq!(c.state, CardState::InProgress);
        assert_eq!(c.claim.as_ref().unwrap().terminal, "t4");
    }

    #[test]
    fn generated_ids_are_short_base36_and_distinct() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            let id = gen_id(&seen);
            assert_eq!(id.len(), 6, "{id}");
            assert!(
                id.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
                "{id}"
            );
            assert!(seen.insert(id), "collision not regenerated");
        }
    }

    #[test]
    fn run_nonce_is_stable_within_the_process() {
        assert_eq!(run_nonce(), run_nonce());
        assert!(!run_nonce().is_empty());
    }

    fn sample_card(body: Option<&str>) -> Card {
        Card::new(
            "a3f8k2".into(),
            "Fix resize flicker".into(),
            body.map(str::to_string),
            "2026-08-28T13:55:00Z".into(),
        )
    }

    #[test]
    fn dispatch_prompt_path_style_renders_the_spec_template_verbatim() {
        let card = sample_card(Some("Resize flickers on Up-arrow."));
        let s = dispatch_prompt(&card, CloseoutStyle::Path);
        assert_eq!(
            s,
            "You are a worker Session dispatched from card a3f8k2 on this project's board.\n\
             \n\
             # Task: Fix resize flicker\n\
             \n\
             Resize flickers on Up-arrow.\n\
             \n\
             # Close-out (required)\n\
             When the work is complete, run:    foreman kanban done a3f8k2\n\
             If you are stuck and need a human: foreman kanban block a3f8k2 --reason \"<one line>\"\n\
             Do not end the session without running one of these."
        );
    }

    #[test]
    fn dispatch_prompt_path_style_renders_with_no_body() {
        let card = sample_card(None);
        let s = dispatch_prompt(&card, CloseoutStyle::Path);
        assert_eq!(
            s,
            "You are a worker Session dispatched from card a3f8k2 on this project's board.\n\
             \n\
             # Task: Fix resize flicker\n\
             \n\
             \n\
             \n\
             # Close-out (required)\n\
             When the work is complete, run:    foreman kanban done a3f8k2\n\
             If you are stuck and need a human: foreman kanban block a3f8k2 --reason \"<one line>\"\n\
             Do not end the session without running one of these."
        );
    }

    #[test]
    fn dispatch_prompt_envvar_style_renders_the_dev_fleet_template_verbatim() {
        let card = sample_card(Some("Resize flickers on Up-arrow."));
        let s = dispatch_prompt(&card, CloseoutStyle::EnvVar);
        assert_eq!(
            s,
            "You are a worker Session dispatched from card a3f8k2 on this project's board.\n\
             \n\
             # Task: Fix resize flicker\n\
             \n\
             Resize flickers on Up-arrow.\n\
             \n\
             # Close-out (required)\n\
             When the work is complete, run:    & $env:FOREMAN_EXE kanban done a3f8k2\n\
             If you are stuck and need a human: & $env:FOREMAN_EXE kanban block a3f8k2 --reason \"<one line>\"\n\
             (bash: write \"$FOREMAN_EXE\" in place of & $env:FOREMAN_EXE)\n\
             Do not end the session without running one of these."
        );
    }

    #[test]
    fn dispatch_prompt_envvar_style_renders_with_no_body() {
        let card = sample_card(None);
        let s = dispatch_prompt(&card, CloseoutStyle::EnvVar);
        assert_eq!(
            s,
            "You are a worker Session dispatched from card a3f8k2 on this project's board.\n\
             \n\
             # Task: Fix resize flicker\n\
             \n\
             \n\
             \n\
             # Close-out (required)\n\
             When the work is complete, run:    & $env:FOREMAN_EXE kanban done a3f8k2\n\
             If you are stuck and need a human: & $env:FOREMAN_EXE kanban block a3f8k2 --reason \"<one line>\"\n\
             (bash: write \"$FOREMAN_EXE\" in place of & $env:FOREMAN_EXE)\n\
             Do not end the session without running one of these."
        );
    }

    #[test]
    fn card_line_json_round_trips_and_carries_the_derived_orphaned_flag() {
        let line = CardLine {
            card: sample_card(None),
            orphaned: true,
        };
        let j = line.json_line();
        assert!(j.contains("\"orphaned\":true"), "{j}");
        let back: CardLine = serde_json::from_str(&j).unwrap();
        assert_eq!(back, line);
    }

    #[test]
    fn card_line_human_line_shows_claim_reason_and_orphan_marker() {
        let mut card = sample_card(None);
        card.state = CardState::InProgress;
        card.claim = Some(Claim {
            terminal: "t4".into(),
            run: "r1".into(),
            agent: Some("claude".into()),
            at: String::new(),
        });
        let line = CardLine {
            card,
            orphaned: true,
        };
        assert_eq!(
            line.human_line(),
            "a3f8k2  in_progress  Fix resize flicker  [t4 claude] ORPHANED"
        );

        let mut blocked = sample_card(None);
        blocked.state = CardState::Blocked;
        blocked.blocked_reason = Some("waiting on design".into());
        let line = CardLine {
            card: blocked,
            orphaned: false,
        };
        assert_eq!(
            line.human_line(),
            "a3f8k2  blocked  Fix resize flicker  (waiting on design)"
        );
    }

    fn line_with_state(id: &str, state: CardState, orphaned: bool) -> CardLine {
        let mut card = Card::new(id.into(), "t".into(), None, "2026-08-28T00:00:00Z".into());
        card.state = state;
        CardLine { card, orphaned }
    }

    #[test]
    fn wait_verdict_by_id_covers_every_row() {
        let mut watched = std::collections::HashSet::new();
        let target = WaitTarget::Id("a1".into());

        let cards = vec![line_with_state("a1", CardState::Done, false)];
        assert_eq!(wait_verdict(&target, &mut watched, &cards), Some(0));

        let cards = vec![line_with_state("a1", CardState::Blocked, false)];
        assert_eq!(wait_verdict(&target, &mut watched, &cards), Some(1));

        let cards = vec![line_with_state("a1", CardState::InProgress, true)];
        assert_eq!(wait_verdict(&target, &mut watched, &cards), Some(1)); // orphaned

        let cards: Vec<CardLine> = vec![]; // removed under the waiter
        assert_eq!(wait_verdict(&target, &mut watched, &cards), Some(1));

        let cards = vec![line_with_state("a1", CardState::Backlog, false)];
        assert_eq!(wait_verdict(&target, &mut watched, &cards), None);

        let cards = vec![line_with_state("a1", CardState::InProgress, false)];
        assert_eq!(wait_verdict(&target, &mut watched, &cards), None);
    }

    #[test]
    fn wait_verdict_any_watches_a_card_only_after_seeing_it_in_progress() {
        let mut watched = std::collections::HashSet::new();
        let target = WaitTarget::Any;

        // Sitting in Backlog: never watched, never triggers even once Done
        // (it skipped being observed InProgress).
        let cards = vec![line_with_state("b1", CardState::Backlog, false)];
        assert_eq!(wait_verdict(&target, &mut watched, &cards), None);
        assert!(watched.is_empty());

        // Now it's seen InProgress: watched, but still in-flight so None.
        let cards = vec![line_with_state("b1", CardState::InProgress, false)];
        assert_eq!(wait_verdict(&target, &mut watched, &cards), None);
        assert!(watched.contains("b1"));

        // Transitions to Done: triggers.
        let cards = vec![line_with_state("b1", CardState::Done, false)];
        assert_eq!(wait_verdict(&target, &mut watched, &cards), Some(0));

        // A separately-watched card going Blocked also triggers.
        let mut watched2 = std::collections::HashSet::new();
        let cards = vec![line_with_state("c1", CardState::InProgress, false)];
        wait_verdict(&target, &mut watched2, &cards);
        let cards = vec![line_with_state("c1", CardState::Blocked, false)];
        assert_eq!(wait_verdict(&target, &mut watched2, &cards), Some(1));

        // And going missing (removed under the waiter) also triggers.
        let mut watched3 = std::collections::HashSet::new();
        let cards = vec![line_with_state("d1", CardState::InProgress, false)];
        wait_verdict(&target, &mut watched3, &cards);
        let cards: Vec<CardLine> = vec![];
        assert_eq!(wait_verdict(&target, &mut watched3, &cards), Some(1));
    }
}
