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

/// How often a shown board re-derives every worktree card's git status on a
/// background thread (spec: dispatch-worktrees §Status poll).
pub const STATUS_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

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

/// A card's private checkout (spec: dispatch-worktrees). Stored on the card
/// so `block` and Restart keep it; status (dirty/ahead/behind/missing) is
/// derived by the poll and never written to the file.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Worktree {
    /// Absolute worktree root, forward slashes: `<root>/.foreman/worktrees/<id>`.
    pub path: String,
    /// Always `card/<id>`.
    pub branch: String,
    /// The branch checked out in the main checkout at dispatch — the
    /// integration target.
    pub base: String,
    /// Branch mode (spec: dispatch-branch): no worktree was made; the card
    /// works on `branch` in the project checkout itself and `path` is that
    /// checkout's root. Absent in the file when false, so worktree cards
    /// are byte-identical to before.
    #[serde(default, skip_serializing_if = "is_false")]
    pub in_place: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Worktree {
    /// The repository root the layout was derived from: `path` minus its
    /// last three components (`.foreman/worktrees/<id>`), or `path` itself
    /// for a branch-mode card.
    pub fn root(&self) -> std::path::PathBuf {
        if self.in_place {
            return std::path::PathBuf::from(&self.path);
        }
        std::path::Path::new(&self.path)
            .ancestors()
            .nth(3)
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default()
    }
}

/// Path and branch naming for a card's worktree. Forward slashes throughout
/// so the stored string matches what `git rev-parse --show-toplevel` prints
/// on Windows and the file is stable across re-saves.
pub fn worktree_layout(root: &std::path::Path, id: &str, base: &str) -> Worktree {
    let root = root.to_string_lossy().replace('\\', "/");
    Worktree {
        path: format!("{}/.foreman/worktrees/{id}", root.trim_end_matches('/')),
        branch: format!("card/{id}"),
        base: base.to_string(),
        in_place: false,
    }
}

/// Naming for a branch-mode card (spec: dispatch-branch): the same
/// `card/<id>` branch, in the project checkout at `root`.
pub fn branch_layout(root: &std::path::Path, id: &str, base: &str) -> Worktree {
    let root = root.to_string_lossy().replace('\\', "/");
    Worktree {
        path: root.trim_end_matches('/').to_string(),
        branch: format!("card/{id}"),
        base: base.to_string(),
        in_place: true,
    }
}

/// Derived worktree state, recomputed by the poll; lives in memory only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct WorktreeStatus {
    pub dirty: bool,
    pub ahead: u32,
    pub behind: u32,
    pub missing: bool,
}

/// `porcelain` is the output of `git status --porcelain --untracked-files=no`
/// inside the tree (`None` = the directory is gone); `rev_list` is the
/// output of `git rev-list --left-right --count <base>...<branch>`
/// (`behind<TAB>ahead`). Unparseable counts read as zero.
pub fn parse_status(porcelain: Option<&str>, rev_list: &str) -> WorktreeStatus {
    let mut it = rev_list.split_whitespace();
    let behind = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let ahead = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    WorktreeStatus {
        dirty: porcelain.is_some_and(|p| !p.trim().is_empty()),
        ahead,
        behind,
        missing: porcelain.is_none(),
    }
}

/// The one-line worktree summary both the card face and `kanban list` show:
/// branch, then `+ahead -behind`, then `dirty` / `missing` flags. No status
/// yet (first poll round pending) renders the branch alone.
pub fn worktree_summary(wt: &Worktree, st: Option<&WorktreeStatus>) -> String {
    let Some(st) = st else {
        return wt.branch.clone();
    };
    let mut s = format!("{} +{} -{}", wt.branch, st.ahead, st.behind);
    if st.dirty {
        s.push_str(" dirty");
    }
    if st.missing {
        s.push_str(" missing");
    }
    s
}

/// A foreman-made worktree (under `<root>/.foreman/worktrees/`) that no
/// card owns any more: the card was removed while its teardown could not
/// run, its file was deleted by hand, or the current branch does not carry
/// it. Derived by the status poll from `git worktree list`; memory only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrayWorktree {
    pub wt: Worktree,
    /// `None` when the probe errored this round (shown as branch alone).
    pub status: Option<WorktreeStatus>,
}

/// The directory name of a foreman worktree — the id of the card it was
/// made for, which is how the board and toasts name a stray.
pub fn worktree_dir_name(wt: &Worktree) -> String {
    wt.path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// `(path, branch)` for every entry of `git worktree list --porcelain`
/// whose path sits under `<root>/.foreman/worktrees/`. The main checkout
/// and hand-made trees elsewhere are dropped. A detached entry reads as
/// branch `HEAD`; a `prunable` entry (directory gone) is kept so the page
/// can show it as missing. Paths are compared like [`same_path`].
pub fn parse_worktree_list(porcelain: &str, root: &str) -> Vec<(String, String)> {
    let prefix = format!(
        "{}/.foreman/worktrees/",
        root.replace('\\', "/").trim_end_matches('/')
    );
    let under = |p: &str| {
        let p = p.replace('\\', "/");
        let (p, prefix) = if cfg!(windows) {
            (p.to_ascii_lowercase(), prefix.to_ascii_lowercase())
        } else {
            (p, prefix.clone())
        };
        p.starts_with(&prefix) && p.len() > prefix.len()
    };
    let mut out = Vec::new();
    for block in porcelain.replace("\r\n", "\n").split("\n\n") {
        let mut path = None;
        let mut branch = None;
        for line in block.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(p.trim().to_string());
            } else if let Some(b) = line.strip_prefix("branch ") {
                branch = Some(
                    b.trim()
                        .strip_prefix("refs/heads/")
                        .unwrap_or(b.trim())
                        .to_string(),
                );
            }
        }
        if let Some(p) = path
            && under(&p)
        {
            out.push((p, branch.unwrap_or_else(|| "HEAD".into())));
        }
    }
    out
}

/// The listed foreman worktrees that no card in `owned` points at.
pub fn strays_among(listed: Vec<Worktree>, owned: &[(String, Worktree)]) -> Vec<Worktree> {
    listed
        .into_iter()
        .filter(|wt| !owned.iter().any(|(_, o)| same_path(&o.path, &wt.path)))
        .collect()
}

/// Who a row on the Worktrees page belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowOwner {
    Card {
        id: String,
        state: CardState,
        title: String,
        /// The Version a shipped Done card sits in.
        version: Option<String>,
        /// The claimed terminal when the claim is live (Open terminal).
        terminal: Option<String>,
    },
    /// No card owns the tree; the directory name is the former card id.
    None,
}

/// One row of the Worktrees page (the board's worktree overview).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeRow {
    pub wt: Worktree,
    pub status: Option<WorktreeStatus>,
    pub owner: RowOwner,
    /// Same rule as the card face: dirty always; ahead of base once the card
    /// has left In Progress (a stray is never in progress).
    pub attention: bool,
}

impl WorktreeRow {
    /// The name the page and toasts use: the card id, or the directory name.
    pub fn name(&self) -> String {
        match &self.owner {
            RowOwner::Card { id, .. } => id.clone(),
            RowOwner::None => worktree_dir_name(&self.wt),
        }
    }
}

/// Column order for the page: live cards first (Backlog, In Progress,
/// Blocked, Done — each in store order), then strays by path.
fn state_rank(s: CardState) -> u8 {
    match s {
        CardState::Backlog => 0,
        CardState::InProgress => 1,
        CardState::Blocked => 2,
        CardState::Done => 3,
    }
}

/// Build the page's rows from the store's view of the world. Pure, so the
/// ordering, labels, and attention rule are unit-tested without git.
pub fn worktree_rows(
    cards: &[Card],
    orphans: &std::collections::HashSet<String>,
    status_of: impl Fn(&str) -> Option<WorktreeStatus>,
    strays: &[StrayWorktree],
) -> Vec<WorktreeRow> {
    let mut owned: Vec<(u8, WorktreeRow)> = cards
        .iter()
        .filter_map(|c| {
            // A branch-mode card has no tree: the page lists worktrees only.
            let wt = c.worktree.clone().filter(|w| !w.in_place)?;
            let st = status_of(&c.id);
            let orphaned = orphans.contains(&c.id);
            let terminal = c
                .claim
                .as_ref()
                .filter(|_| c.state == CardState::InProgress && !orphaned)
                .map(|cl| cl.terminal.clone());
            let attention =
                st.is_some_and(|s| s.dirty || (s.ahead > 0 && c.state != CardState::InProgress));
            Some((
                state_rank(c.state),
                WorktreeRow {
                    wt,
                    status: st,
                    owner: RowOwner::Card {
                        id: c.id.clone(),
                        state: c.state,
                        title: c.title.clone(),
                        version: c.shipped.as_ref().map(|s| s.name.clone()),
                        terminal,
                    },
                    attention,
                },
            ))
        })
        .collect();
    owned.sort_by_key(|(rank, _)| *rank);
    let mut rows: Vec<WorktreeRow> = owned.into_iter().map(|(_, r)| r).collect();
    let mut strays: Vec<WorktreeRow> = strays
        .iter()
        .map(|s| WorktreeRow {
            wt: s.wt.clone(),
            status: s.status,
            owner: RowOwner::None,
            attention: s.status.is_some_and(|st| st.dirty || st.ahead > 0),
        })
        .collect();
    strays.sort_by(|a, b| a.wt.path.cmp(&b.wt.path));
    rows.extend(strays);
    rows
}

/// Rows for `foreman kanban worktrees`: every tree a card points at plus
/// every foreman tree git lists, each probed once by `probe` (`None` = the
/// probe errored; the line shows the branch alone). Pure apart from the
/// injected probe, so the union and the stray split are unit-tested.
pub fn live_worktree_rows(
    cards: &[Card],
    orphans: &std::collections::HashSet<String>,
    listed: Vec<Worktree>,
    probe: impl Fn(&Worktree) -> Option<WorktreeStatus>,
) -> Vec<WorktreeRow> {
    let owned: Vec<(String, Worktree)> = cards
        .iter()
        .filter_map(|c| {
            c.worktree
                .clone()
                .filter(|w| !w.in_place)
                .map(|w| (c.id.clone(), w))
        })
        .collect();
    let by_card: std::collections::HashMap<String, Option<WorktreeStatus>> = owned
        .iter()
        .map(|(id, wt)| (id.clone(), probe(wt)))
        .collect();
    let strays: Vec<StrayWorktree> = strays_among(listed, &owned)
        .into_iter()
        .map(|wt| StrayWorktree {
            status: probe(&wt),
            wt,
        })
        .collect();
    worktree_rows(
        cards,
        orphans,
        |id| by_card.get(id).copied().flatten(),
        &strays,
    )
}

/// The card a worktree line belongs to (`foreman kanban worktrees --json`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorktreeLineCard {
    pub id: String,
    pub state: CardState,
    pub title: String,
}

/// One line of `foreman kanban worktrees`: the wire shape (`--json`, one
/// object per line) and the human line. `card` is absent for a stray;
/// `status` is absent when the probe errored.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorktreeLine {
    /// The directory name: the card id the tree was made for.
    pub name: String,
    pub path: String,
    pub branch: String,
    pub base: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<WorktreeLineCard>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<WorktreeStatus>,
}

impl WorktreeLine {
    pub fn from_row(row: &WorktreeRow) -> Self {
        let card = match &row.owner {
            RowOwner::Card {
                id, state, title, ..
            } => Some(WorktreeLineCard {
                id: id.clone(),
                state: *state,
                title: title.clone(),
            }),
            RowOwner::None => None,
        };
        Self {
            name: worktree_dir_name(&row.wt),
            path: row.wt.path.clone(),
            branch: row.wt.branch.clone(),
            base: row.wt.base.clone(),
            card,
            status: row.status,
        }
    }

    pub fn json_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// `<name>  <state>  <title>  [wt <summary>]` for a card's tree,
    /// `<name>  no card  [wt <summary>]` for a stray — the same `[wt …]`
    /// tail `kanban list` prints, so eyes and scripts learn one format.
    pub fn human_line(&self) -> String {
        let wt = Worktree {
            path: self.path.clone(),
            branch: self.branch.clone(),
            base: self.base.clone(),
            in_place: false,
        };
        let tail = format!("[wt {}]", worktree_summary(&wt, self.status.as_ref()));
        match &self.card {
            Some(c) => {
                let state = match c.state {
                    CardState::Backlog => "backlog",
                    CardState::InProgress => "in_progress",
                    CardState::Blocked => "blocked",
                    CardState::Done => "done",
                };
                format!("{}  {state}  {}  {tail}", self.name, c.title)
            }
            None => format!("{}  no card  {tail}", self.name),
        }
    }
}

/// The board's bottom strip: `N worktrees · D dirty · S no card`, zero
/// segments omitted; `None` when there is nothing to show. The bool is
/// attention (any dirty or stray).
pub fn worktree_strip(rows: &[WorktreeRow]) -> Option<(String, bool)> {
    if rows.is_empty() {
        return None;
    }
    let dirty = rows
        .iter()
        .filter(|r| r.status.is_some_and(|s| s.dirty))
        .count();
    let stray = rows.iter().filter(|r| r.owner == RowOwner::None).count();
    let mut s = format!(
        "{} {}",
        rows.len(),
        if rows.len() == 1 {
            "worktree"
        } else {
            "worktrees"
        }
    );
    if dirty > 0 {
        s.push_str(&format!(" · {dirty} dirty"));
    }
    if stray > 0 {
        s.push_str(&format!(" · {stray} no card"));
    }
    Some((s, dirty > 0 || stray > 0))
}

/// The Version a Done card was Cut into (spec: kanban-cut §Card schema).
/// Set only by [`CardStore::cut`], cleared only by [`CardStore::uncut`].
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Shipped {
    /// Free-form, stored as typed (trimmed). Compared only via [`same_name`].
    pub name: String,
    /// The Cut timestamp, shared by every card in the Cut — the Version's
    /// only sort key. `updated` is NOT: a later Discard bumps `updated` and
    /// must not reorder the Version.
    pub at: String,
    /// Abbreviated shas, oldest first, of commits whose `Card: <id>` trailer
    /// named this card at Cut time. Frozen at Cut; omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commits: Vec<String>,
}

/// The pinned dropdown literal for the live Done column; refused as a
/// Version name in any case.
pub const CURRENT: &str = "Current";

/// The one place Version names compare: trimmed, case-insensitive.
pub fn same_name(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// One row of the Done dropdown (spec §Board UI).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub name: String,
    pub at: String,
    pub count: usize,
}

/// Distinct `shipped.name`s, newest Cut first (by `at`, then name). Two
/// spellings that `same_name` equates are one row, spelled as first seen.
pub fn versions(cards: &[Card]) -> Vec<Version> {
    let mut out: Vec<Version> = Vec::new();
    for s in cards.iter().filter_map(|c| c.shipped.as_ref()) {
        match out.iter_mut().find(|v| same_name(&v.name, &s.name)) {
            Some(v) => {
                v.count += 1;
                if s.at > v.at {
                    v.at = s.at.clone();
                }
            }
            None => out.push(Version {
                name: s.name.clone(),
                at: s.at.clone(),
                count: 1,
            }),
        }
    }
    out.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.name.cmp(&b.name)));
    out
}

/// The plan and wave a card belongs to (spec: plan-view §Data shape).
/// One optional field, so a card is in at most one plan by construction —
/// there is nothing to validate. Set only by `kanban edit --plan/--wave`;
/// `--plan ""` clears it. The board and the card's `state` are untouched:
/// this is ordering metadata, not a column.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Planned {
    /// Free-form, stored as typed (trimmed). Compared only via [`same_name`],
    /// exactly like a Version name.
    pub name: String,
    /// Ordering only — lower runs earlier. Numbers need not be contiguous,
    /// and a gap is just a gap. A hand-edited file that omits it reads as
    /// wave 1, the same wave `--plan` alone assigns.
    #[serde(default = "first_wave")]
    pub wave: u32,
}

/// The wave `--plan` alone assigns, and what a `wave`-less file reads as.
fn first_wave() -> u32 {
    1
}

/// The slice of a [`Card`] the plan view reads. Whole `Card` clones would
/// drag every card's `body` along once per frame for nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanCard {
    pub id: String,
    pub title: String,
    pub state: CardState,
}

/// One wave of a plan. Cards in a wave are a set, not a list — the order
/// below is just `created`, the same order the board shows them in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wave {
    pub number: u32,
    pub cards: Vec<PlanCard>,
}

/// A plan, derived from card fields every time it is asked for. Nothing is
/// stored: this rebuilds from [`Card::planned`] exactly the way [`versions`]
/// rebuilds the Done dropdown from `Card::shipped`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Spelled as first seen; two spellings [`same_name`] equates are one
    /// plan.
    pub name: String,
    /// Ascending by number.
    pub waves: Vec<Wave>,
    /// The newest `created` among the plan's cards — the sort key, so the
    /// plan you are still adding cards to stays on top.
    pub at: String,
}

impl Plan {
    /// The current wave: the lowest number still holding a non-Done card
    /// (spec §The window). `None` once every card is Done — the plan is
    /// finished, not stuck on its last wave.
    pub fn current(&self) -> Option<u32> {
        self.waves
            .iter()
            .find(|w| w.cards.iter().any(|c| c.state != CardState::Done))
            .map(|w| w.number)
    }

    /// Total cards across every wave. A plan of one is almost always a
    /// misspelled plan name (`same_name` folds case and outer whitespace
    /// only), so the view says so rather than drawing it as a real plan.
    pub fn card_count(&self) -> usize {
        self.waves.iter().map(|w| w.cards.len()).sum()
    }
}

/// Every plan the cards describe, newest activity first (by `at`, then
/// name) — the sibling of [`versions`].
///
/// Cards keep the order they arrive in within a wave, which for
/// [`CardStore::cards`] is `created` ascending (see `sort_cards`); this
/// function does not re-sort them, the same contract the board relies on.
pub fn plans(cards: &[Card]) -> Vec<Plan> {
    let mut out: Vec<Plan> = Vec::new();
    for c in cards {
        let Some(p) = c.planned.as_ref() else {
            continue;
        };
        let i = match out.iter().position(|x| same_name(&x.name, &p.name)) {
            Some(i) => i,
            None => {
                out.push(Plan {
                    name: p.name.clone(),
                    waves: Vec::new(),
                    at: c.created.clone(),
                });
                out.len() - 1
            }
        };
        let plan = &mut out[i];
        if c.created > plan.at {
            plan.at = c.created.clone();
        }
        let w = match plan.waves.iter().position(|w| w.number == p.wave) {
            Some(w) => w,
            None => {
                plan.waves.push(Wave {
                    number: p.wave,
                    cards: Vec::new(),
                });
                plan.waves.len() - 1
            }
        };
        plan.waves[w].cards.push(PlanCard {
            id: c.id.clone(),
            title: c.title.clone(),
            state: c.state,
        });
    }
    for p in &mut out {
        p.waves.sort_by_key(|w| w.number);
    }
    out.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.name.cmp(&b.name)));
    out
}

/// Pure half of the trailer walk (spec §Cut step 6). Input is
/// `git log --format=%h%x09%(trailers:key=Card,valueonly,separator=%x2C)`
/// output, newest first; output is card id -> shas, oldest first. A commit
/// with no trailer is skipped; one naming several cards lands in each.
pub fn parse_trailer_log(text: &str) -> std::collections::HashMap<String, Vec<String>> {
    let mut out: std::collections::HashMap<String, Vec<String>> = Default::default();
    for line in text.lines().rev() {
        let Some((sha, ids)) = line.split_once('\t') else {
            continue;
        };
        for id in ids.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            out.entry(id.to_string())
                .or_default()
                .push(sha.trim().to_string());
        }
    }
    out
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
    /// Absent unless the card was dispatched into a worktree (spec:
    /// dispatch-worktrees). Survives `block`; cleared by teardown, not `done`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<Worktree>,
    /// Absent until the card is Cut into a Version (spec: kanban-cut).
    /// `state` stays `done`; this is grouping, not a column.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shipped: Option<Shipped>,
    /// Absent unless the card was tagged into a plan (spec: plan-view).
    /// Ordering metadata only — `state` and the board are untouched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planned: Option<Planned>,
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
            worktree: None,
            shipped: None,
            planned: None,
            created: now.clone(),
            updated: now,
        }
    }

    /// Load-time repair: a `shipped` or `planned` whose name trims to empty
    /// (a hand edit) is no Version and no plan at all.
    fn normalize(mut self) -> Self {
        if self
            .shipped
            .as_ref()
            .is_some_and(|s| s.name.trim().is_empty())
        {
            self.shipped = None;
        }
        if self
            .planned
            .as_ref()
            .is_some_and(|p| p.name.trim().is_empty())
        {
            self.planned = None;
        }
        self
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
pub(crate) fn now_stamp() -> String {
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
    /// Last poll round's derived worktree status per card id (spec:
    /// dispatch-worktrees §Status poll). Memory only, replaced wholesale.
    worktree_status: std::collections::HashMap<String, WorktreeStatus>,
    /// Last poll round's foreman worktrees that no card owns. Memory only.
    strays: Vec<StrayWorktree>,
    last_status_poll: Option<std::time::Instant>,
    status_inflight: bool,
    /// The integration queue's view of each card (spec:
    /// worktree-integration-queue), refreshed by the project's coordinator
    /// tick from the repository's request files. Memory only, replaced
    /// wholesale; never written to a card file.
    integration: std::collections::HashMap<String, crate::integrate::IntegrationView>,
}

/// What one Cut did (spec §Cut step 9). `held_back` is `(id, reason)` for
/// candidates left in Current.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CutOutcome {
    pub name: String,
    pub shipped: Vec<String>,
    pub held_back: Vec<(String, String)>,
}

impl CutOutcome {
    /// The reply/toast lines: the summary, then one line per held-back card.
    pub fn lines(&self) -> Vec<String> {
        let mut v = vec![format!("cut {}: {} cards", self.name, self.shipped.len())];
        for (id, why) in &self.held_back {
            v.push(format!("{id} stayed in Current ({why})"));
        }
        v
    }
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
                            Ok(card) => cards.push(card.normalize()),
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

    pub fn worktree_status(&self, id: &str) -> Option<WorktreeStatus> {
        self.worktree_status.get(id).copied()
    }

    /// Replace the derived status map wholesale (one poll round) and release
    /// the in-flight latch so the next round may start.
    pub fn set_worktree_statuses(
        &mut self,
        mut map: std::collections::HashMap<String, WorktreeStatus>,
        mut strays: Vec<StrayWorktree>,
    ) {
        // The round was snapshotted before it ran; a card whose worktree
        // was cleared (teardown) or removed meanwhile must not get a status
        // back, or `list --json` would print `worktree_status` without
        // `worktree` until the next round.
        map.retain(|id, _| {
            self.cards
                .iter()
                .any(|c| c.id == *id && c.worktree.is_some())
        });
        // Likewise a tree a card claimed since the snapshot is not a stray.
        strays.retain(|s| {
            !self.cards.iter().any(|c| {
                c.worktree
                    .as_ref()
                    .is_some_and(|w| same_path(&w.path, &s.wt.path))
            })
        });
        self.worktree_status = map;
        self.strays = strays;
        self.status_inflight = false;
    }

    /// Foreman worktrees no card owns, from the last poll round.
    pub fn strays(&self) -> &[StrayWorktree] {
        &self.strays
    }

    /// The queue's current view of `id`'s integration request, if any.
    pub fn integration(&self, id: &str) -> Option<crate::integrate::IntegrationView> {
        self.integration.get(id).cloned()
    }

    /// Replace the integration views wholesale (one coordinator read).
    pub fn set_integration(
        &mut self,
        map: std::collections::HashMap<String, crate::integrate::IntegrationView>,
    ) {
        self.integration = map;
    }

    /// When a round is due — board shown, no round in flight, interval
    /// elapsed — return the card batch to poll (possibly empty: the round
    /// still lists strays) and latch in-flight. The caller runs git on a
    /// background thread and answers with [`Self::set_worktree_statuses`].
    pub fn take_status_poll(&mut self, now: std::time::Instant) -> Option<Vec<(String, Worktree)>> {
        if !self.shown_recently(now) || self.status_inflight {
            return None;
        }
        if let Some(last) = self.last_status_poll {
            if now.duration_since(last) < STATUS_POLL_INTERVAL {
                return None;
            }
        }
        let batch: Vec<(String, Worktree)> = self
            .cards
            .iter()
            .filter_map(|c| c.worktree.clone().map(|w| (c.id.clone(), w)))
            .collect();
        if batch.is_empty() {
            self.worktree_status.clear();
        }
        self.last_status_poll = Some(now);
        self.status_inflight = true;
        Some(batch)
    }

    /// Drop the worktree field after a successful teardown. Missing card =
    /// error (an `rm`'d card's teardown simply has nothing to clear).
    pub fn clear_worktree(&mut self, id: &str) -> Result<(), String> {
        let dir = self.dir_or_err()?.to_path_buf();
        let mut card = self.read_one(id)?;
        card.worktree = None;
        card.updated = now_stamp();
        self.write_card(&dir, &card)?;
        self.replace_in_memory(card);
        self.worktree_status.remove(id);
        Ok(())
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
        serde_json::from_str::<Card>(&text)
            .map(Card::normalize)
            .map_err(|e| format!("card {id} is corrupt: {e}"))
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

    /// Replace title, body, and/or plan membership on an existing card.
    /// Allowed in any state; does not touch claim or column. Body is a full
    /// replace, not an append. At least one field must be `Some`; a provided
    /// title is trimmed and must be non-empty. Missing card = error (never
    /// created).
    ///
    /// Plan rules (spec: plan-view §Authoring): `plan` is trimmed and an
    /// empty one clears the card's plan; `wave` alone re-numbers a card that
    /// already has a plan and errors on one that does not, because a wave
    /// with no plan orders nothing.
    pub fn edit(
        &mut self,
        id: &str,
        title: Option<&str>,
        body: Option<&str>,
        plan: Option<&str>,
        wave: Option<u32>,
    ) -> Result<(), String> {
        if title.is_none() && body.is_none() && plan.is_none() && wave.is_none() {
            return Err("edit requires --title, --body, --plan and/or --wave".into());
        }
        let title = match title {
            Some(t) => {
                let t = t.trim();
                if t.is_empty() {
                    return Err("title cannot be empty".into());
                }
                Some(t)
            }
            None => None,
        };
        let plan = plan.map(str::trim);
        if plan.is_some_and(str::is_empty) && wave.is_some() {
            return Err("--plan \"\" clears the plan; --wave has nothing to set".into());
        }
        let dir = self.dir_or_err()?.to_path_buf();
        let mut card = self.read_one(id)?;
        if let Some(t) = title {
            card.title = t.to_string();
        }
        if let Some(b) = body {
            card.body = Some(b.to_string());
        }
        if let Some(name) = plan {
            card.planned = if name.is_empty() {
                None
            } else {
                Some(Planned {
                    name: name.to_string(),
                    // `--plan X --wave N` lands here with the wave already
                    // known; `--plan X` alone starts at wave 1.
                    wave: wave.unwrap_or_else(first_wave),
                })
            };
        }
        if let Some(w) = wave {
            match &mut card.planned {
                Some(pl) => pl.wave = w,
                None => return Err("--wave needs a plan: set --plan first".into()),
            }
        }
        card.updated = now_stamp();
        self.write_card(&dir, &card)?;
        self.replace_in_memory(card);
        Ok(())
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
        worktree: Option<Worktree>,
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
        // `Some` records a fresh bring-up; `None` leaves any recorded tree
        // alone so a Restart or self-service `start` keeps it.
        if let Some(wt) = worktree {
            card.worktree = Some(wt);
        }
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
        self.claim_common(id, terminal, None, current_run, term, None)
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
        worktree: Option<Worktree>,
    ) -> Result<(), String> {
        self.claim_common(id, terminal, Some(agent), current_run, term, worktree)
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

    /// Cut (spec §Cut). Everything is judged against the files (`reload`
    /// first, like `add`), all validation runs before the first write, and a
    /// failed write reverts the cards already stamped. The two facts that
    /// need the project are injected so this store never runs git:
    /// `hold` is asked about each candidate that still carries a worktree
    /// and answers `Some(reason)` to leave it in Current; `commits` maps
    /// the surviving candidates' ids to their trailer shas.
    pub fn cut(
        &mut self,
        name: &str,
        hold: impl Fn(&Card) -> Option<String>,
        commits: impl FnOnce(&[Card]) -> std::collections::HashMap<String, Vec<String>>,
    ) -> Result<CutOutcome, String> {
        let dir = self.dir_or_err()?.to_path_buf();
        let name = name.trim();
        if name.is_empty() {
            return Err("cut needs a version name".into());
        }
        if same_name(name, CURRENT) {
            return Err(format!(
                "{CURRENT} is the live Done column, not a version name"
            ));
        }
        self.reload();
        if self
            .cards
            .iter()
            .any(|c| c.shipped.as_ref().is_some_and(|s| same_name(&s.name, name)))
        {
            return Err(format!(
                "version {name} already exists; uncut it first or pick a new name"
            ));
        }
        let candidates: Vec<Card> = self
            .cards
            .iter()
            .filter(|c| c.state == CardState::Done && c.shipped.is_none())
            .cloned()
            .collect();
        if candidates.is_empty() {
            return Err("nothing in Done to cut".into());
        }
        let mut held_back = Vec::new();
        let mut keep: Vec<Card> = Vec::new();
        for c in candidates {
            // Only a card with a worktree can be provably unmerged; one
            // worked in the main checkout is always a candidate.
            let why = if c.worktree.is_some() { hold(&c) } else { None };
            match why {
                Some(why) => held_back.push((c.id.clone(), why)),
                None => keep.push(c),
            }
        }
        if keep.is_empty() {
            return Err("no card in Done is merged; nothing to cut".into());
        }
        let mut by_id = commits(&keep);
        let at = now_stamp();
        let originals = keep.clone();
        for c in &mut keep {
            c.shipped = Some(Shipped {
                name: name.to_string(),
                at: at.clone(),
                commits: by_id.remove(&c.id).unwrap_or_default(),
            });
            c.updated = at.clone();
        }
        if let Err((i, e)) = self.write_batch(&dir, &keep) {
            // `originals[..i]` were rewritten with the stamp; put them back.
            // `keep[i]` never landed (tmp write or rename failed).
            let revert = self.write_batch(&dir, &originals[..i]);
            self.reload();
            let failed = &keep[i].id;
            return Err(match revert {
                Ok(()) => format!("cut {name} failed on {failed}: {e}; nothing shipped"),
                Err((j, e2)) => format!(
                    "cut {name} failed on {failed}: {e}; revert also failed on {}: {e2}; run uncut {name}",
                    originals[j].id
                ),
            });
        }
        self.reload();
        Ok(CutOutcome {
            name: name.to_string(),
            shipped: keep.iter().map(|c| c.id.clone()).collect(),
            held_back,
        })
    }

    /// Uncut (spec §Uncut): clear `shipped` on every card in the Version,
    /// case-insensitively. They reappear in Current Done.
    pub fn uncut(&mut self, name: &str) -> Result<usize, String> {
        let dir = self.dir_or_err()?.to_path_buf();
        self.reload();
        let mut cards: Vec<Card> = self
            .cards
            .iter()
            .filter(|c| c.shipped.as_ref().is_some_and(|s| same_name(&s.name, name)))
            .cloned()
            .collect();
        if cards.is_empty() {
            return Err(format!("no version named {name}"));
        }
        let now = now_stamp();
        for c in &mut cards {
            c.shipped = None;
            c.updated = now.clone();
        }
        let n = cards.len();
        let res = self.write_batch(&dir, &cards);
        self.reload();
        res.map_err(|(i, e)| format!("uncut {name} failed on {}: {e}", cards[i].id))?;
        Ok(n)
    }

    /// Write cards in order; stop at the first failure and say which index.
    /// One `reload` afterwards (the callers') replaces N per-card
    /// fingerprint scans.
    fn write_batch(&self, dir: &std::path::Path, cards: &[Card]) -> Result<(), (usize, String)> {
        for (i, c) in cards.iter().enumerate() {
            self.write_card(dir, c).map_err(|e| (i, e))?;
        }
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
/// fields interpolated, nothing else. Two things vary: the close-out CLI
/// form, by [`CloseoutStyle`] (installed-on-PATH vs. dev-fleet), and — only
/// when the card carries a worktree — a `# Workspace` section plus the
/// style-independent integration lines (spec: dispatch-worktrees). A card
/// without a worktree renders exactly the pre-worktree text.
pub fn dispatch_prompt(card: &Card, style: CloseoutStyle) -> String {
    let mut out = format!(
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
    if let Some(wt) = &card.worktree
        && wt.in_place
    {
        // Branch mode (spec: dispatch-branch): no private tree. The human's
        // uncommitted changes may share the checkout, so the prompt forbids
        // every command that would sweep them into the card or move them.
        out.push_str(&format!(
            "# Workspace\n\
             You are in the project checkout at {path}, on branch {branch}, created from {base}. There is no worktree.\n\
             This checkout may hold uncommitted changes that are not yours. Stage only the files you changed, by name: never git add -A or git add ., never stash, reset, restore, clean, or switch branches.\n\
             Leave .foreman/ untouched and never stage it.\n\
             \n",
            path = wt.path,
            branch = wt.branch,
            base = wt.base,
        ));
    } else if let Some(wt) = &card.worktree {
        out.push_str(&format!(
            "# Workspace\n\
             You are in a git worktree at {path}, on branch {branch}, based on {base}.\n\
             The main checkout at {root} is shared with other workers: never edit files there.\n\
             Leave .foreman/ untouched and never stage it.\n\
             \n",
            path = wt.path,
            branch = wt.branch,
            base = wt.base,
            root = wt.root().display(),
        ));
    }
    out.push_str("# Close-out (required)\n");
    // The card trailer (spec: kanban-cut §The card trailer): the only
    // per-commit signal that survives rebase and squash, read back at Cut.
    out.push_str(&format!(
        "End every commit message with the trailer line:    Card: {id}\n",
        id = card.id
    ));
    let cli = match style {
        CloseoutStyle::Path => "foreman",
        CloseoutStyle::EnvVar => "& $env:FOREMAN_EXE",
    };
    if let Some(wt) = &card.worktree
        && wt.in_place
    {
        // Branch mode integrates through the same queue, but in place: the
        // queue never rebases the shared checkout, so a moved base comes
        // back to the worker (spec: dispatch-branch §Integration).
        out.push_str(&format!(
            "Commit your work on {branch}, then hand integration to Foreman's queue (never merge or switch branches yourself):\n\
             \x20   {cli} kanban integrate {id}\n\
             \x20   {cli} kanban wait {id} --timeout 1800\n\
             wait exit 0: Foreman ran the project checks, fast-forwarded {base} to your branch, put the checkout back on {base}, and marked the card Done. You are finished; do not run done yourself.\n\
             wait exit 3: a check failed or {base} moved. Read the reason with {cli} kanban list --json (the \"integration\" object), fix it here (for a moved base: git rebase {base}, resolving any conflicts; if git refuses because of uncommitted changes that are not yours, block instead), commit, then run integrate and wait again. Queued is not Done.\n\
             wait exit 2: still queued or checking; run wait again.\n",
            id = card.id,
            branch = wt.branch,
            base = wt.base,
        ));
    } else if let Some(wt) = &card.worktree {
        // The integration queue (spec: worktree-integration-queue): the
        // worker submits and waits; Foreman rebases, checks, fast-forwards,
        // and marks the card Done. The worker never merges into the shared
        // checkout, and queued is not Done.
        out.push_str(&format!(
            "Commit everything in your worktree, then hand integration to Foreman's queue (never merge into the main checkout yourself):\n\
             \x20   {cli} kanban integrate {id}\n\
             \x20   {cli} kanban wait {id} --timeout 1800\n\
             wait exit 0: Foreman rebased onto {base}, ran the project checks, fast-forwarded {base}, and marked the card Done. You are finished; do not run done yourself.\n\
             wait exit 3: the rebase conflicted or a check failed. Read the reason with {cli} kanban list --json (the \"integration\" object), fix it in your worktree (finish the rebase, commit), then run integrate and wait again. Queued is not Done.\n\
             wait exit 2: still queued or checking; run wait again.\n",
            id = card.id,
            base = wt.base,
        ));
    } else {
        out.push_str(&format!(
            "When the work is complete, run:    {cli} kanban done {id}\n",
            id = card.id
        ));
    }
    out.push_str(&format!(
        "If you are stuck and need a human: {cli} kanban block {id} --reason \"<one line>\"\n",
        id = card.id
    ));
    if style == CloseoutStyle::EnvVar {
        out.push_str("(bash: write \"$FOREMAN_EXE\" in place of & $env:FOREMAN_EXE)\n");
    }
    out.push_str(if card.worktree.is_some() {
        "Do not end the session without the card Done (by the queue) or blocked."
    } else {
        "Do not end the session without running one of these."
    });
    out
}

/// A card plus the derived `orphaned` flag (never stored in the card file
/// itself — see [`is_orphaned`]) for the agent- and human-facing list views.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CardLine {
    #[serde(flatten)]
    pub card: Card,
    pub orphaned: bool,
    /// Derived from the store's last poll round; absent for cards without a
    /// worktree and for cards not yet polled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_status: Option<WorktreeStatus>,
    /// The integration queue's view of the card (spec:
    /// worktree-integration-queue); absent when no request stands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integration: Option<crate::integrate::IntegrationView>,
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
        if let Some(wt) = &self.card.worktree {
            // `br` marks a branch-mode card: no tree, the project checkout.
            tail.push(format!(
                "[{} {}]",
                if wt.in_place { "br" } else { "wt" },
                worktree_summary(wt, self.worktree_status.as_ref())
            ));
        }
        if let Some(i) = &self.integration {
            tail.push(i.tail());
        }
        if let Some(s) = &self.card.shipped {
            tail.push(format!("[shipped {}]", s.name));
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

/// Exit code of `foreman kanban wait <id>` when the card's integration was
/// handed back (spec: worktree-integration-queue): the worker must resolve
/// and resubmit. Distinct from `1` (needs a human) so a worker waiting on
/// its own card can branch on it.
pub const WAIT_NEEDS_RESOLUTION: i32 = 3;

/// Pure verdict function the CLI wait loop (Task 2) drives on every poll.
/// `Id`: Done -> exit 0; Blocked/orphaned/missing -> exit 1 (something needs
/// a human); integration handed back -> exit [`WAIT_NEEDS_RESOLUTION`];
/// Backlog, live InProgress, queued, or integrating -> keep waiting.
/// `Any`: every live (non-orphaned) InProgress card is added to `watched` on
/// sight, then each watched id is checked the same way — a card sitting in
/// Backlog is never watched and so never triggers. `Any` ignores handed-back
/// integrations: that is the live worker's job, not the orchestrator's.
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
                _ if c
                    .integration
                    .as_ref()
                    .is_some_and(|i| i.phase == crate::integrate::Phase::NeedsResolution) =>
                {
                    Some(WAIT_NEEDS_RESOLUTION)
                }
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

// ---------------------------------------------------------------------------
// Git plumbing for per-card worktrees (spec: dispatch-worktrees). Every call
// is a short-lived `git` subprocess with no console window. Bring-up runs
// synchronously in the dispatch drain; teardown and the status poll run on
// background threads (wm.rs owns the threads and the channel).
// ---------------------------------------------------------------------------

/// Run `git -C <cwd> <args>`; `Ok(stdout trimmed)` on exit 0, otherwise
/// `Err(first stderr line)` (or a spawn error). Never opens a console window.
pub(crate) fn git(cwd: &std::path::Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(cwd).args(args);
    cmd.stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().map_err(|e| format!("cannot run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let first = err
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim();
        if first.is_empty() {
            Err(format!(
                "git {} failed ({})",
                args.first().unwrap_or(&""),
                out.status
            ))
        } else {
            Err(first.to_string())
        }
    }
}

/// True when `git` answers `--version` — the skip gate for git-backed tests.
/// Production never asks: a missing git makes `rev-parse` fail, which
/// `bring_up_worktree` already reads as "dispatch in place".
#[cfg(test)]
pub fn git_available() -> bool {
    git(std::path::Path::new("."), &["--version"]).is_ok()
}

/// Where a card-dispatched worker runs — the per-dispatch choice the board
/// offers (seeded from `Settings::dispatch_worktrees`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchMode {
    /// A private worktree under `.foreman/worktrees/<id>` on `card/<id>`.
    Worktree,
    /// `card/<id>` in the project checkout itself (spec: dispatch-branch).
    Branch,
    /// The project checkout on whatever branch it has; no git at all.
    InPlace,
}

impl DispatchMode {
    /// The choice a card already carrying a record must restart with: its
    /// tree, or its branch. `None` for a card without one.
    pub fn locked_for(card: &Card) -> Option<Self> {
        card.worktree.as_ref().map(|w| {
            if w.in_place {
                DispatchMode::Branch
            } else {
                DispatchMode::Worktree
            }
        })
    }

    /// The board chip's cycle: worktree → branch → in place → worktree.
    pub fn next(self) -> Self {
        match self {
            DispatchMode::Worktree => DispatchMode::Branch,
            DispatchMode::Branch => DispatchMode::InPlace,
            DispatchMode::InPlace => DispatchMode::Worktree,
        }
    }

    /// Short chip label (the inline picker has room for a word).
    pub fn chip(self) -> &'static str {
        match self {
            DispatchMode::Worktree => "wt",
            DispatchMode::Branch => "branch",
            DispatchMode::InPlace => "here",
        }
    }

    /// The mode's name in toasts and on the detail page.
    pub fn name(self) -> &'static str {
        match self {
            DispatchMode::Worktree => "worktree",
            DispatchMode::Branch => "branch",
            DispatchMode::InPlace => "in place",
        }
    }

    /// The chip's hover text and the detail page's option text, naming the
    /// branch card `id` would get (the same `card/<id>` the layouts use).
    pub fn describe(self, id: &str) -> String {
        match self {
            DispatchMode::Worktree => format!("A private git worktree on card/{id}"),
            DispatchMode::Branch => format!(
                "Branch card/{id} in the project checkout (no worktree; uncommitted changes stay put)"
            ),
            DispatchMode::InPlace => "The project checkout as it is (no branch)".into(),
        }
    }
}

/// What bring-up decided for one dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BringUp {
    /// Spawn the worker with this worktree as its cwd and record it.
    Worktree(Worktree),
    /// Not a git repository: dispatch in the project cwd, no toast.
    InPlace,
    /// Detached HEAD: dispatch in the project cwd with a warning toast.
    Detached,
}

/// Two strings name the same place. Slash-normalized; case-insensitive on
/// Windows (drive letters and user dirs vary in case between `rev-parse`
/// and the porcelain listing). When both paths exist, filesystem identity
/// wins: GitHub Actions Windows often has tempfile as `C:\Users\RUNNER~1\…`
/// while `git worktree list` prints `C:/Users/runneradmin/…`, and a
/// string compare would list an owned tree as a stray.
pub(crate) fn same_path(a: &str, b: &str) -> bool {
    let na = a.replace('\\', "/");
    let nb = b.replace('\\', "/");
    let strings_match = if cfg!(windows) {
        na.eq_ignore_ascii_case(&nb)
    } else {
        na == nb
    };
    if strings_match {
        return true;
    }
    match (
        std::fs::canonicalize(std::path::Path::new(a)),
        std::fs::canonicalize(std::path::Path::new(b)),
    ) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}

/// Spec §Bring-up, steps 1–5. Any git failure is `Err` with git's first
/// stderr line; the caller aborts before spawning. Reuse is decided by
/// `git worktree list`, not by the card's field, so a claim that failed
/// after a successful bring-up cannot strand a tree.
pub fn bring_up_worktree(project_cwd: &std::path::Path, card: &Card) -> Result<BringUp, String> {
    // 1. repo root (not a repo → in place, silently)
    let Ok(root) = git(project_cwd, &["rev-parse", "--show-toplevel"]) else {
        return Ok(BringUp::InPlace);
    };
    let root = std::path::PathBuf::from(root);
    // 2. base branch (detached → in place, with a warning upstream)
    let Ok(base) = git(&root, &["symbolic-ref", "--short", "HEAD"]) else {
        return Ok(BringUp::Detached);
    };
    // A branch-mode card has the checkout on its branch: basing a tree on
    // that would integrate this card into the other card's branch.
    refuse_card_branch(&base, &card.id)?;
    // 3. naming; a card that already carries this worktree keeps its base
    let mut wt = worktree_layout(&root, &card.id, &base);
    if let Some(prev) = &card.worktree {
        if same_path(&prev.path, &wt.path) {
            wt.base = prev.base.clone();
        }
    }
    // 4. ignore the directory per-clone (info/exclude), never via .gitignore
    if git(&root, &["check-ignore", "-q", &wt.path]).is_err() {
        let exclude = git(&root, &["rev-parse", "--git-path", "info/exclude"])?;
        let exclude = std::path::PathBuf::from(exclude);
        let exclude = if exclude.is_absolute() {
            exclude
        } else {
            root.join(exclude)
        };
        if let Some(parent) = exclude.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        let mut text = std::fs::read_to_string(&exclude).unwrap_or_default();
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(".foreman/worktrees/\n");
        std::fs::write(&exclude, text)
            .map_err(|e| format!("cannot write {}: {e}", exclude.display()))?;
    }
    // 5. reuse or create
    let listed = git(&root, &["worktree", "list", "--porcelain"])?;
    let already = listed
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .any(|p| same_path(p, &wt.path));
    if already {
        return Ok(BringUp::Worktree(wt));
    }
    let branch_ref = format!("refs/heads/{}", wt.branch);
    if git(&root, &["rev-parse", "--verify", "--quiet", &branch_ref]).is_ok() {
        git(&root, &["worktree", "add", &wt.path, &wt.branch])?;
    } else {
        git(
            &root,
            &["worktree", "add", "-b", &wt.branch, &wt.path, "HEAD"],
        )?;
    }
    Ok(BringUp::Worktree(wt))
}

/// The checkout is on another card's branch (a live branch-mode card):
/// refuse, naming the card, rather than stack one card on another.
fn refuse_card_branch(current: &str, id: &str) -> Result<(), String> {
    match current.strip_prefix("card/") {
        Some(other) if other != id => Err(format!(
            "the project checkout is on {current}, card {other}'s branch; integrate or release that card first"
        )),
        _ => Ok(()),
    }
}

/// Branch-mode bring-up (spec: dispatch-branch §Bring-up). Puts the project
/// checkout on `card/<id>` without making a worktree. Only non-forcing git
/// verbs touch the checkout, so uncommitted changes are never lost:
///
/// - already on `card/<id>` → reuse (Restart), no git write;
/// - `card/<id>` exists → `git switch card/<id>`, which refuses when the
///   switch would overwrite a local change (the error aborts the dispatch);
/// - otherwise → `git switch -c card/<id>` from HEAD, which never touches the
///   working tree: the changes stay where they are, uncommitted.
///
/// Not a repository → `InPlace`; detached HEAD → `Detached` (same as the
/// worktree bring-up). A git operation in progress, or the checkout on
/// another card's branch, is `Err`.
pub fn bring_up_branch(project_cwd: &std::path::Path, card: &Card) -> Result<BringUp, String> {
    let Ok(root) = git(project_cwd, &["rev-parse", "--show-toplevel"]) else {
        return Ok(BringUp::InPlace);
    };
    let root = std::path::PathBuf::from(root);
    let Ok(current) = git(&root, &["symbolic-ref", "--short", "HEAD"]) else {
        return Ok(BringUp::Detached);
    };
    refuse_card_branch(&current, &card.id)?;
    if let Some(op) = crate::integrate::git_op_in_progress(&root) {
        return Err(format!(
            "a git {op} is in progress in the project checkout; finish it first"
        ));
    }
    let mut wt = branch_layout(&root, &card.id, &current);
    // Restart keeps the base recorded at first dispatch: the checkout is on
    // the card's own branch (or on whatever the human switched to) now.
    if let Some(prev) = &card.worktree
        && prev.in_place
        && same_path(&prev.path, &wt.path)
    {
        wt.base = prev.base.clone();
    }
    if current == wt.branch {
        return Ok(BringUp::Worktree(wt));
    }
    let branch_ref = format!("refs/heads/{}", wt.branch);
    if git(&root, &["rev-parse", "--verify", "--quiet", &branch_ref]).is_ok() {
        git(&root, &["switch", &wt.branch])?;
    } else {
        git(&root, &["switch", "-c", &wt.branch])?;
    }
    Ok(BringUp::Worktree(wt))
}

/// Tracked files with uncommitted changes in the checkout at `root`,
/// outside `.foreman/` (the app writes card files there). Branch-mode
/// dispatch warns with the count; empty on any git failure.
pub fn checkout_changes(root: &std::path::Path) -> Vec<String> {
    checkout_changes_strict(root)
        .map(|s| {
            s.lines()
                .filter_map(|l| l.trim_start().split_once(' '))
                .map(|(_, p)| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// `git status --porcelain` of tracked files outside `.foreman/`; fails
/// closed (the status probe must not read a git error as clean).
fn checkout_changes_strict(root: &std::path::Path) -> Result<String, String> {
    git(
        root,
        &[
            "status",
            "--porcelain",
            "--untracked-files=no",
            "--",
            ".",
            ":(exclude).foreman",
        ],
    )
}

/// Live status probe (spec §Status poll): dirty from inside the tree,
/// ahead/behind from any checkout of the repo. Also the synchronous `rm`
/// pre-check. A vanished directory reads as `missing` with counts intact.
///
/// Fails CLOSED: a git error (no binary, lock contention, bad ref) is an
/// `Err`, never a clean-looking zero status — `rm` refuses on it rather than
/// deleting a card whose worktree it could not inspect.
pub fn worktree_status_now(
    project_cwd: &std::path::Path,
    wt: &Worktree,
) -> Result<WorktreeStatus, String> {
    // A checkout always has a `.git` entry. Without one (an empty leftover
    // of a partial removal) `git status` run inside the directory would
    // climb to the main repository and report ITS changes as the card's.
    let tree = std::path::Path::new(&wt.path);
    let porcelain = if wt.in_place {
        // Branch mode: the checkout's changes count as the card's only
        // while the checkout is on the card's branch; `.foreman/` is the
        // app's own churn. The checkout itself is never "missing".
        let on = git(tree, &["symbolic-ref", "--short", "-q", "HEAD"]).unwrap_or_default();
        if on == wt.branch {
            Some(checkout_changes_strict(tree)?)
        } else {
            Some(String::new())
        }
    } else if tree.join(".git").exists() {
        Some(git(
            tree,
            &["status", "--porcelain", "--untracked-files=no"],
        )?)
    } else {
        None
    };
    let range = format!("{}...{}", wt.base, wt.branch);
    let rev_list = git(
        project_cwd,
        &["rev-list", "--left-right", "--count", &range],
    )?;
    Ok(parse_status(porcelain.as_deref(), &rev_list))
}

/// Every foreman worktree git knows about, for the board's overview.
/// `base` is whatever the main checkout has checked out now (`HEAD` when
/// detached) — a stray has no card to remember its base. Not a repository,
/// or any git failure, is `Err`; the poll then lists nothing.
pub fn foreman_worktrees_now(project_cwd: &std::path::Path) -> Result<Vec<Worktree>, String> {
    let root = git(project_cwd, &["rev-parse", "--show-toplevel"])?;
    let base = git(
        &std::path::PathBuf::from(&root),
        &["symbolic-ref", "--short", "HEAD"],
    )
    .unwrap_or_else(|_| "HEAD".into());
    let listed = git(project_cwd, &["worktree", "list", "--porcelain"])?;
    Ok(parse_worktree_list(&listed, &root)
        .into_iter()
        .map(|(path, branch)| Worktree {
            path,
            branch,
            base: base.clone(),
            in_place: false,
        })
        .collect())
}

/// Commits reachable from HEAD, committed since `since` (RFC3339), carrying
/// a `Card:` trailer — bucketed by card id, oldest first (spec §Cut step 6).
/// Fail-open: any git failure (no git, not a repo, no commits) is an empty
/// map, never an error. A project without git still Cuts, just without
/// commits.
pub fn trailer_commits(
    cwd: &std::path::Path,
    since: &str,
) -> std::collections::HashMap<String, Vec<String>> {
    let since = format!("--since={since}");
    match git(
        cwd,
        &[
            "log",
            "HEAD",
            &since,
            "--format=%h%x09%(trailers:key=Card,valueonly,separator=%x2C)",
        ],
    ) {
        Ok(text) => parse_trailer_log(&text),
        Err(_) => Default::default(),
    }
}

/// Newest `v*` tag by version order, for the Cut field's prefill (spec
/// §Cut, Board). Fail-open: no git, no repo, or no tags is `None`.
pub fn latest_v_tag(cwd: &std::path::Path) -> Option<String> {
    git(cwd, &["tag", "-l", "v*", "--sort=-v:refname"])
        .ok()?
        .lines()
        .next()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Spec §Teardown outcome table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TeardownOutcome {
    /// Tree and branch both gone; clear the card's `worktree` field.
    Removed,
    /// `worktree remove` refused: uncommitted changes. Everything kept.
    Dirty,
    /// `branch -d` refused: commits not on base. Tree removed, branch kept.
    Unmerged { ahead: u32 },
    /// git missing or an unexpected error; everything kept.
    Failed(String),
}

/// Pure classification of the two git results. `branch` is `None` when the
/// branch step never ran (the remove failed first). `ahead` is the count
/// reported for the Unmerged row.
pub fn teardown_verdict(
    remove: Result<(), String>,
    branch: Option<Result<(), String>>,
    ahead: u32,
) -> TeardownOutcome {
    match remove {
        Err(e) if e.to_lowercase().contains("modified or untracked") => TeardownOutcome::Dirty,
        Err(e) => TeardownOutcome::Failed(e),
        Ok(()) => match branch {
            None => TeardownOutcome::Failed("branch step did not run".into()),
            Some(Ok(())) => TeardownOutcome::Removed,
            Some(Err(e)) if e.to_lowercase().contains("not fully merged") => {
                TeardownOutcome::Unmerged { ahead }
            }
            Some(Err(e)) => TeardownOutcome::Failed(e),
        },
    }
}

/// Spec §Teardown: remove the tree, delete the branch, prune. `force` is the
/// human-only Discard path (`remove --force`, `branch -D`). Safe to repeat
/// after a partial earlier run: each step judges what is left rather than
/// assuming the previous state, so a tree or branch that is already gone
/// reads as that step completed.
pub fn teardown_worktree(
    project_cwd: &std::path::Path,
    wt: &Worktree,
    force: bool,
) -> TeardownOutcome {
    if wt.in_place {
        return teardown_branch(project_cwd, wt, force);
    }
    // Count only; a failed probe reads as 0 here because the outcome below
    // is decided by git's own refusal, not by this number.
    let ahead = worktree_status_now(project_cwd, wt)
        .map(|s| s.ahead)
        .unwrap_or(0);
    let remove = remove_tree(project_cwd, wt, force);
    let branch = remove
        .is_ok()
        .then(|| delete_branch(project_cwd, &wt.branch, force));
    let _ = git(project_cwd, &["worktree", "prune"]);
    teardown_verdict(remove, branch, ahead)
}

/// Branch-mode teardown (spec: dispatch-branch §Teardown): there is no tree
/// to remove — the checkout is the project's — so only the branch goes.
/// A checkout still on the card's branch is first moved back to base with a
/// non-forcing `git switch`, and only when the branch is merged into base
/// (or under Discard): an unmerged branch is never switched away from
/// behind the human's back. `switch` carries uncommitted changes along and
/// refuses when it would overwrite one — that reads as `Dirty`.
fn teardown_branch(project_cwd: &std::path::Path, wt: &Worktree, force: bool) -> TeardownOutcome {
    let root = wt.root();
    let ahead = worktree_status_now(project_cwd, wt)
        .map(|s| s.ahead)
        .unwrap_or(0);
    let on = git(&root, &["symbolic-ref", "--short", "-q", "HEAD"]).unwrap_or_default();
    if on == wt.branch {
        let merged = git(
            &root,
            &["merge-base", "--is-ancestor", &wt.branch, &wt.base],
        )
        .is_ok();
        if !merged && !force {
            return TeardownOutcome::Unmerged { ahead };
        }
        if let Err(e) = git(&root, &["switch", &wt.base]) {
            return if e.to_lowercase().contains("would be overwritten") {
                TeardownOutcome::Dirty
            } else {
                TeardownOutcome::Failed(e)
            };
        }
    }
    match delete_branch(&root, &wt.branch, force) {
        Ok(()) => TeardownOutcome::Removed,
        Err(e) if e.to_lowercase().contains("not fully merged") => {
            TeardownOutcome::Unmerged { ahead }
        }
        Err(e) => TeardownOutcome::Failed(e),
    }
}

/// Is `path` a working tree git still tracks? Fails closed: an `Err` when
/// git cannot answer, never a guess from the filesystem.
fn worktree_registered(project_cwd: &std::path::Path, path: &str) -> Result<bool, String> {
    let listed = git(project_cwd, &["worktree", "list", "--porcelain"])?;
    Ok(listed
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .any(|p| same_path(p, path)))
}

/// Teardown step 1. Registration and the directory are judged separately
/// because `git worktree remove` is not atomic: on Windows a directory that
/// is some process's cwd survives the final rmdir after git has already
/// deleted its contents AND its registration. Retrying git on that leftover
/// fails forever ("not a working tree"), so:
///
/// - registered + directory present → `git worktree remove` (retried
///   briefly for a transient hold; a refusal that leaves the path
///   unregistered falls through to the leftover rule);
/// - registered + directory gone → `git worktree prune`;
/// - unregistered → the leftover rule: remove the directory only when it is
///   EMPTY and sits under `<root>/.foreman/worktrees/`. Git no longer tracks
///   it, so anything inside it is not ours to delete — a nonempty leftover
///   is kept and reported, `--force` or not.
fn remove_tree(project_cwd: &std::path::Path, wt: &Worktree, force: bool) -> Result<(), String> {
    let tree = std::path::Path::new(&wt.path);
    if worktree_registered(project_cwd, &wt.path)? {
        if !tree.is_dir() {
            return git(project_cwd, &["worktree", "prune"]).map(|_| ());
        }
        match git_worktree_remove(project_cwd, &wt.path, force) {
            Ok(()) => return Ok(()),
            Err(e) if is_dirty_refusal(&e) => return Err(e),
            Err(e) => {
                // Git may have torn the tree down and only failed on the top
                // directory; judge what is left rather than the exit code.
                let _ = git(project_cwd, &["worktree", "prune"]);
                if worktree_registered(project_cwd, &wt.path)? {
                    return Err(e);
                }
            }
        }
    }
    remove_leftover_dir(project_cwd, tree)
}

fn is_dirty_refusal(err: &str) -> bool {
    err.to_lowercase().contains("modified or untracked")
}

/// `git worktree remove [--force] <path>`. A process that just exited inside
/// the tree (the worker's shell) can hold the directory for a moment on
/// Windows; a refusal that is neither "dirty" nor a partial removal (path
/// no longer registered) is retried briefly.
fn git_worktree_remove(
    project_cwd: &std::path::Path,
    path: &str,
    force: bool,
) -> Result<(), String> {
    let args: &[&str] = if force {
        &["worktree", "remove", "--force", path]
    } else {
        &["worktree", "remove", path]
    };
    let mut attempt = 0;
    loop {
        attempt += 1;
        match git(project_cwd, args) {
            Ok(_) => break Ok(()),
            Err(e) if attempt < 5 && !is_dirty_refusal(&e) => {
                std::thread::sleep(std::time::Duration::from_millis(400));
                if !std::path::Path::new(path).is_dir() {
                    break Ok(());
                }
                if !worktree_registered(project_cwd, path)? {
                    break Err(e);
                }
                continue;
            }
            Err(e) => break Err(e),
        }
    }
}

/// The leftover rule (see `remove_tree`). Non-recursive on purpose: an
/// unregistered directory with contents is never deleted, and the path must
/// be the one bring-up would have chosen for this repository so a card file
/// edited by hand cannot aim teardown at an arbitrary empty directory.
fn remove_leftover_dir(
    project_cwd: &std::path::Path,
    tree: &std::path::Path,
) -> Result<(), String> {
    if !tree.exists() {
        return Ok(());
    }
    let shown = tree.display();
    let root = git(project_cwd, &["rev-parse", "--show-toplevel"])?;
    let expected = std::path::Path::new(&root)
        .join(".foreman")
        .join("worktrees");
    let in_place = tree
        .parent()
        .is_some_and(|p| same_path(&p.to_string_lossy(), &expected.to_string_lossy()));
    if !in_place {
        return Err(format!(
            "leftover directory is outside {}; delete it by hand: {shown}",
            expected.display()
        ));
    }
    let nonempty = std::fs::read_dir(tree)
        .map_err(|e| format!("cannot read leftover directory {shown}: {e}"))?
        .next()
        .is_some();
    if nonempty {
        return Err(format!(
            "leftover directory is not empty and no longer a git worktree; inspect and delete it by hand: {shown}"
        ));
    }
    // The same momentary cwd hold that stranded the directory can still be
    // on it; give it the same brief grace as the git path.
    let mut attempt = 0;
    loop {
        attempt += 1;
        match std::fs::remove_dir(tree) {
            Ok(()) => break Ok(()),
            Err(_) if !tree.exists() => break Ok(()),
            Err(_) if attempt < 5 => {
                std::thread::sleep(std::time::Duration::from_millis(400));
            }
            Err(e) => break Err(format!("cannot remove leftover directory {shown}: {e}")),
        }
    }
}

/// Teardown step 2. A branch that is already gone (an earlier run got this
/// far, or the human deleted it) is a completed step, not an error. An
/// existing branch keeps git's own merged-branch protection: `-d` refuses
/// when it is not fully merged, `-D` (Discard) does not.
fn delete_branch(project_cwd: &std::path::Path, branch: &str, force: bool) -> Result<(), String> {
    if git(project_cwd, &["branch", "--list", branch])?.is_empty() {
        return Ok(());
    }
    let flag = if force { "-D" } else { "-d" };
    git(project_cwd, &["branch", flag, branch]).map(|_| ())
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
    fn edit_replaces_title_and_or_body_without_touching_state_or_claim() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_at(tmp.path());
        let id = store.add("old title", Some("old body")).unwrap();

        // Prove `updated` actually moves: stamp the file in the past, then edit.
        let path = tmp
            .path()
            .join(".foreman")
            .join("tasks")
            .join(format!("{id}.json"));
        let mut raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        raw["updated"] = "2020-01-01T00:00:00Z".into();
        std::fs::write(&path, serde_json::to_string_pretty(&raw).unwrap()).unwrap();
        store.reload();

        store
            .edit(&id, Some("  new title  "), None, None, None)
            .unwrap();
        let card = store.get(&id).unwrap();
        assert_eq!(card.title, "new title");
        assert_eq!(card.body.as_deref(), Some("old body"));
        assert_eq!(card.state, CardState::Backlog);
        assert!(card.claim.is_none());
        assert_ne!(card.updated, "2020-01-01T00:00:00Z");

        store
            .edit(&id, None, Some("replacement"), None, None)
            .unwrap();
        let card = store.get(&id).unwrap();
        assert_eq!(card.title, "new title");
        assert_eq!(card.body.as_deref(), Some("replacement"));

        let run = run_nonce();
        store.start(&id, "t1", run, TermState::Running).unwrap();
        let claim = store.get(&id).unwrap().claim.clone();
        store
            .edit(&id, Some("still in progress"), Some("both"), None, None)
            .unwrap();
        let card = store.get(&id).unwrap();
        assert_eq!(card.title, "still in progress");
        assert_eq!(card.body.as_deref(), Some("both"));
        assert_eq!(card.state, CardState::InProgress);
        assert_eq!(card.claim, claim);

        assert!(store.edit(&id, None, None, None, None).is_err());
        assert!(store.edit(&id, Some("   "), None, None, None).is_err());
        assert_eq!(store.get(&id).unwrap().title, "still in progress");
        assert!(store.edit("nope00", Some("x"), None, None, None).is_err());
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
             End every commit message with the trailer line:    Card: a3f8k2\n\
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
             End every commit message with the trailer line:    Card: a3f8k2\n\
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
             End every commit message with the trailer line:    Card: a3f8k2\n\
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
             End every commit message with the trailer line:    Card: a3f8k2\n\
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
            worktree_status: None,
            integration: None,
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
            worktree_status: None,
            integration: None,
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
            worktree_status: None,
            integration: None,
        };
        assert_eq!(
            line.human_line(),
            "a3f8k2  blocked  Fix resize flicker  (waiting on design)"
        );
    }

    fn line_with_state(id: &str, state: CardState, orphaned: bool) -> CardLine {
        let mut card = Card::new(id.into(), "t".into(), None, "2026-08-28T00:00:00Z".into());
        card.state = state;
        CardLine {
            card,
            orphaned,
            worktree_status: None,
            integration: None,
        }
    }

    // --- dispatch-worktrees: git plumbing (skipped without git on PATH) ---

    #[test]
    fn teardown_verdict_covers_the_four_row_table() {
        let dirty = Err(
            "fatal: 'x' contains modified or untracked files, use --force to delete it".to_string(),
        );
        let unmerged = Err("error: the branch 'card/a' is not fully merged".to_string());
        let other = Err("fatal: something else".to_string());
        assert_eq!(
            teardown_verdict(Ok(()), Some(Ok(())), 0),
            TeardownOutcome::Removed
        );
        assert_eq!(
            teardown_verdict(dirty.clone(), None, 0),
            TeardownOutcome::Dirty
        );
        assert_eq!(
            teardown_verdict(Ok(()), Some(unmerged.clone()), 2),
            TeardownOutcome::Unmerged { ahead: 2 }
        );
        assert_eq!(
            teardown_verdict(other.clone(), None, 0),
            TeardownOutcome::Failed("fatal: something else".into())
        );
        assert_eq!(
            teardown_verdict(Ok(()), Some(other), 0),
            TeardownOutcome::Failed("fatal: something else".into())
        );
        // a remove failure decides regardless of a (never-run) branch step
        assert_eq!(
            teardown_verdict(dirty, Some(Ok(())), 0),
            TeardownOutcome::Dirty
        );
        assert!(matches!(
            teardown_verdict(Ok(()), None, 0),
            TeardownOutcome::Failed(_)
        ));
    }

    fn git_in(dir: &std::path::Path, args: &[&str]) -> String {
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
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// `git init -b main` + one tracked file, identity passed inline so the
    /// test never depends on the machine's git config. `None` = no git.
    fn git_repo() -> Option<tempfile::TempDir> {
        if !git_available() {
            eprintln!("git not on PATH; skipping");
            return None;
        }
        let tmp = tempfile::tempdir().unwrap();
        git_in(tmp.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(tmp.path().join("f.txt"), "one\n").unwrap();
        git_in(tmp.path(), &["add", "f.txt"]);
        git_in(tmp.path(), &["commit", "-q", "-m", "init"]);
        Some(tmp)
    }

    #[test]
    fn status_probe_fails_closed_when_git_cannot_answer() {
        // `rm` decides on this verdict; a git failure must be an error, not a
        // clean-looking zero status that lets the card file be deleted.
        if !git_available() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap(); // not a repository
        let wt = Worktree {
            path: tmp.path().join("nope").to_string_lossy().into_owned(),
            branch: "card/a1b2c3".into(),
            base: "main".into(),
            in_place: false,
        };
        assert!(worktree_status_now(tmp.path(), &wt).is_err());
    }

    #[test]
    fn bring_up_creates_a_listed_worktree_on_the_card_branch_and_excludes_it() {
        let Some(repo) = git_repo() else { return };
        let card = Card::new("a1b2c3".into(), "t".into(), None, now_stamp());
        let BringUp::Worktree(wt) = bring_up_worktree(repo.path(), &card).unwrap() else {
            panic!("expected a worktree");
        };
        assert_eq!(wt.branch, "card/a1b2c3");
        assert_eq!(wt.base, "main");
        assert!(
            wt.path.ends_with("/.foreman/worktrees/a1b2c3"),
            "{}",
            wt.path
        );
        assert!(std::path::Path::new(&wt.path).join("f.txt").exists());
        let listed = git_in(repo.path(), &["worktree", "list", "--porcelain"]);
        assert!(
            listed.to_lowercase().contains(&wt.path.to_lowercase()),
            "{listed}"
        );
        assert_eq!(
            git_in(
                std::path::Path::new(&wt.path),
                &["symbolic-ref", "--short", "HEAD"]
            ),
            "card/a1b2c3"
        );
        // ignored locally via info/exclude, never via .gitignore
        assert!(!repo.path().join(".gitignore").exists());
        let status = git_in(repo.path(), &["status", "--porcelain"]);
        assert!(status.is_empty(), "worktree dir must be ignored: {status}");
        // status probe: clean, 0/0
        assert_eq!(
            worktree_status_now(repo.path(), &wt).unwrap(),
            WorktreeStatus::default()
        );
    }

    fn branch_card(id: &str) -> Card {
        Card::new(id.into(), "t".into(), None, now_stamp())
    }

    fn on_branch(dir: &std::path::Path) -> String {
        git_in(dir, &["symbolic-ref", "--short", "HEAD"])
    }

    #[test]
    fn branch_bring_up_switches_the_checkout_and_keeps_uncommitted_changes() {
        let Some(repo) = git_repo() else { return };
        // The human's work in progress: a tracked edit and an untracked file.
        std::fs::write(repo.path().join("f.txt"), "human edit\n").unwrap();
        std::fs::write(repo.path().join("scratch.txt"), "notes\n").unwrap();
        let card = branch_card("b1b2b3");
        let BringUp::Worktree(wt) = bring_up_branch(repo.path(), &card).unwrap() else {
            panic!("expected a branch record");
        };
        assert!(wt.in_place);
        assert_eq!(wt.branch, "card/b1b2b3");
        assert_eq!(wt.base, "main");
        assert!(same_path(&wt.path, &repo.path().to_string_lossy()));
        assert!(same_path(
            &wt.root().to_string_lossy(),
            &repo.path().to_string_lossy()
        ));
        assert_eq!(on_branch(repo.path()), "card/b1b2b3");
        assert_eq!(
            std::fs::read_to_string(repo.path().join("f.txt")).unwrap(),
            "human edit\n"
        );
        assert!(repo.path().join("scratch.txt").exists());
        assert_eq!(checkout_changes(repo.path()), vec!["f.txt".to_string()]);
        // No worktree was made.
        let listed = git_in(repo.path(), &["worktree", "list", "--porcelain"]);
        assert_eq!(listed.matches("worktree ").count(), 1, "{listed}");
        // Restart while on the branch: reuse, base kept, no git write.
        let mut again = card.clone();
        again.worktree = Some(wt.clone());
        let BringUp::Worktree(wt2) = bring_up_branch(repo.path(), &again).unwrap() else {
            panic!("expected reuse");
        };
        assert_eq!(wt2, wt);
    }

    #[test]
    fn branch_bring_up_restarts_on_an_existing_branch_and_refuses_to_clobber() {
        let Some(repo) = git_repo() else { return };
        let card = branch_card("c1c2c3");
        let BringUp::Worktree(wt) = bring_up_branch(repo.path(), &card).unwrap() else {
            panic!("expected a branch record");
        };
        std::fs::write(repo.path().join("f.txt"), "card edit\n").unwrap();
        git_in(repo.path(), &["commit", "-q", "-am", "card work"]);
        git_in(repo.path(), &["switch", "-q", "main"]);
        // Restart from main: switches back onto the existing branch.
        let mut restart = card.clone();
        restart.worktree = Some(wt.clone());
        bring_up_branch(repo.path(), &restart).unwrap();
        assert_eq!(on_branch(repo.path()), "card/c1c2c3");
        // From main with a local edit to the file the branch changed: git
        // refuses the switch, the edit survives, the checkout stays put.
        git_in(repo.path(), &["switch", "-q", "main"]);
        std::fs::write(repo.path().join("f.txt"), "human edit\n").unwrap();
        let err = bring_up_branch(repo.path(), &restart).unwrap_err();
        assert!(err.contains("overwritten"), "{err}");
        assert_eq!(on_branch(repo.path()), "main");
        assert_eq!(
            std::fs::read_to_string(repo.path().join("f.txt")).unwrap(),
            "human edit\n"
        );
    }

    #[test]
    fn dispatch_refuses_to_stack_on_another_cards_branch() {
        let Some(repo) = git_repo() else { return };
        bring_up_branch(repo.path(), &branch_card("d1d2d3")).unwrap();
        let err = bring_up_branch(repo.path(), &branch_card("e1e2e3")).unwrap_err();
        assert!(err.contains("card d1d2d3's branch"), "{err}");
        let err = bring_up_worktree(repo.path(), &branch_card("e1e2e3")).unwrap_err();
        assert!(err.contains("card d1d2d3's branch"), "{err}");
        assert_eq!(on_branch(repo.path()), "card/d1d2d3");
    }

    #[test]
    fn branch_status_counts_the_checkout_only_while_it_is_on_the_branch() {
        let Some(repo) = git_repo() else { return };
        let BringUp::Worktree(wt) = bring_up_branch(repo.path(), &branch_card("f1f2f3")).unwrap()
        else {
            panic!("expected a branch record");
        };
        std::fs::write(repo.path().join("g.txt"), "g\n").unwrap();
        git_in(repo.path(), &["add", "g.txt"]);
        git_in(repo.path(), &["commit", "-q", "-m", "g"]);
        std::fs::write(repo.path().join("f.txt"), "dirty\n").unwrap();
        let st = worktree_status_now(repo.path(), &wt).unwrap();
        assert_eq!(
            (st.dirty, st.ahead, st.behind, st.missing),
            (true, 1, 0, false)
        );
        git_in(repo.path(), &["switch", "-q", "main"]);
        let st = worktree_status_now(repo.path(), &wt).unwrap();
        assert_eq!(
            (st.dirty, st.ahead),
            (false, 1),
            "off the branch, the checkout's changes are not the card's"
        );
    }

    #[test]
    fn branch_teardown_deletes_only_the_branch_and_never_strands_work() {
        let Some(repo) = git_repo() else { return };
        let BringUp::Worktree(wt) = bring_up_branch(repo.path(), &branch_card("a9a9a9")).unwrap()
        else {
            panic!("expected a branch record");
        };
        std::fs::write(repo.path().join("g.txt"), "g\n").unwrap();
        git_in(repo.path(), &["add", "g.txt"]);
        git_in(repo.path(), &["commit", "-q", "-m", "g"]);
        std::fs::write(repo.path().join("f.txt"), "human edit\n").unwrap();
        // Unmerged and checked out: kept, checkout untouched.
        assert_eq!(
            teardown_worktree(repo.path(), &wt, false),
            TeardownOutcome::Unmerged { ahead: 1 }
        );
        assert_eq!(on_branch(repo.path()), "card/a9a9a9");
        // Merged (base fast-forwarded by hand): switched back, branch gone,
        // the uncommitted edit rides along.
        let tip = git_in(repo.path(), &["rev-parse", "HEAD"]);
        git_in(repo.path(), &["update-ref", "refs/heads/main", &tip]);
        assert_eq!(
            teardown_worktree(repo.path(), &wt, false),
            TeardownOutcome::Removed
        );
        assert_eq!(on_branch(repo.path()), "main");
        assert!(git_in(repo.path(), &["branch", "--list", "card/a9a9a9"]).is_empty());
        assert_eq!(
            std::fs::read_to_string(repo.path().join("f.txt")).unwrap(),
            "human edit\n"
        );
        assert!(
            repo.path().join(".git").exists(),
            "the checkout is never removed"
        );
        // Repeating is harmless.
        assert_eq!(
            teardown_worktree(repo.path(), &wt, false),
            TeardownOutcome::Removed
        );
    }

    #[test]
    fn branch_discard_switches_to_base_and_drops_unmerged_commits() {
        let Some(repo) = git_repo() else { return };
        let BringUp::Worktree(wt) = bring_up_branch(repo.path(), &branch_card("b9b9b9")).unwrap()
        else {
            panic!("expected a branch record");
        };
        std::fs::write(repo.path().join("g.txt"), "g\n").unwrap();
        git_in(repo.path(), &["add", "g.txt"]);
        git_in(repo.path(), &["commit", "-q", "-m", "g"]);
        std::fs::write(repo.path().join("scratch.txt"), "notes\n").unwrap();
        assert_eq!(
            teardown_worktree(repo.path(), &wt, true),
            TeardownOutcome::Removed
        );
        assert_eq!(on_branch(repo.path()), "main");
        assert!(git_in(repo.path(), &["branch", "--list", "card/b9b9b9"]).is_empty());
        assert!(repo.path().join("scratch.txt").exists());
    }

    #[test]
    fn dispatch_mode_labels_name_the_cards_real_branch() {
        assert_eq!(
            DispatchMode::Worktree.describe("ab12cd"),
            "A private git worktree on card/ab12cd"
        );
        assert!(
            DispatchMode::Branch
                .describe("ab12cd")
                .starts_with("Branch card/ab12cd in the project checkout")
        );
        assert!(!DispatchMode::InPlace.describe("ab12cd").contains("card/"));
    }

    #[test]
    fn branch_cards_never_appear_on_the_worktrees_page() {
        let mut card = branch_card("c9c9c9");
        card.worktree = Some(branch_layout(
            std::path::Path::new("C:/r"),
            "c9c9c9",
            "main",
        ));
        let rows = worktree_rows(
            std::slice::from_ref(&card),
            &Default::default(),
            |_| None,
            &[],
        );
        assert!(rows.is_empty());
        let rows = live_worktree_rows(&[card], &Default::default(), Vec::new(), |_| None);
        assert!(rows.is_empty());
    }

    #[test]
    fn branch_record_round_trips_and_a_worktree_record_omits_the_flag() {
        let wt = branch_layout(std::path::Path::new("C:\\r"), "d9d9d9", "main");
        assert_eq!(wt.path, "C:/r");
        let text = serde_json::to_string(&wt).unwrap();
        assert!(text.contains("\"in_place\":true"), "{text}");
        assert_eq!(serde_json::from_str::<Worktree>(&text).unwrap(), wt);
        let tree = worktree_layout(std::path::Path::new("C:/r"), "d9d9d9", "main");
        let text = serde_json::to_string(&tree).unwrap();
        assert!(!text.contains("in_place"), "{text}");
    }

    #[test]
    fn dispatch_prompt_for_a_branch_card_guards_the_shared_checkout() {
        let mut card = branch_card("e9e9e9");
        card.title = "Branch work".into();
        card.worktree = Some(branch_layout(
            std::path::Path::new("C:/r"),
            "e9e9e9",
            "main",
        ));
        let p = dispatch_prompt(&card, CloseoutStyle::Path);
        for want in [
            "You are in the project checkout at C:/r, on branch card/e9e9e9, created from main. There is no worktree.",
            "never git add -A or git add ., never stash, reset, restore, clean, or switch branches.",
            "    foreman kanban integrate e9e9e9\n",
            "    foreman kanban wait e9e9e9 --timeout 1800\n",
            "put the checkout back on main",
            "for a moved base: git rebase main",
            "Do not end the session without the card Done (by the queue) or blocked.",
        ] {
            assert!(p.contains(want), "missing {want:?} in:\n{p}");
        }
        assert!(!p.contains("kanban done"), "{p}");
        assert!(!p.contains("git worktree at"), "{p}");
    }

    #[test]
    fn bring_up_is_in_place_outside_a_repo_and_detached_without_a_branch() {
        if !git_available() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let card = Card::new("a1b2c3".into(), "t".into(), None, now_stamp());
        assert!(matches!(
            bring_up_worktree(tmp.path(), &card).unwrap(),
            BringUp::InPlace
        ));
        let Some(repo) = git_repo() else { return };
        let head = git_in(repo.path(), &["rev-parse", "HEAD"]);
        git_in(repo.path(), &["checkout", "-q", "--detach", &head]);
        assert!(matches!(
            bring_up_worktree(repo.path(), &card).unwrap(),
            BringUp::Detached
        ));
    }

    #[test]
    fn bring_up_reuses_an_existing_tree_and_readds_a_leftover_branch() {
        let Some(repo) = git_repo() else { return };
        let mut card = Card::new("a1b2c3".into(), "t".into(), None, now_stamp());
        let BringUp::Worktree(wt) = bring_up_worktree(repo.path(), &card).unwrap() else {
            panic!()
        };
        card.worktree = Some(wt.clone());
        let tree = std::path::Path::new(&wt.path);
        // Restart: same tree, uncommitted state intact
        std::fs::write(tree.join("f.txt"), "edited\n").unwrap();
        let BringUp::Worktree(again) = bring_up_worktree(repo.path(), &card).unwrap() else {
            panic!()
        };
        assert_eq!(again, wt);
        assert_eq!(
            std::fs::read_to_string(tree.join("f.txt")).unwrap(),
            "edited\n"
        );
        assert!(worktree_status_now(repo.path(), &wt).unwrap().dirty);
        // branch left behind by an unmerged teardown: re-add on the branch
        git_in(tree, &["checkout", "-q", "--", "f.txt"]);
        git_in(repo.path(), &["worktree", "remove", &wt.path]);
        assert!(!tree.exists());
        let BringUp::Worktree(third) = bring_up_worktree(repo.path(), &card).unwrap() else {
            panic!()
        };
        assert_eq!(third.path, wt.path);
        assert!(tree.exists());
        assert_eq!(
            git_in(tree, &["symbolic-ref", "--short", "HEAD"]),
            "card/a1b2c3"
        );
    }

    #[test]
    fn teardown_after_merge_leaves_nothing() {
        let Some(repo) = git_repo() else { return };
        let card = Card::new("a1b2c3".into(), "t".into(), None, now_stamp());
        let BringUp::Worktree(wt) = bring_up_worktree(repo.path(), &card).unwrap() else {
            panic!()
        };
        let tree = std::path::Path::new(&wt.path);
        std::fs::write(tree.join("f.txt"), "two\n").unwrap();
        git_in(tree, &["commit", "-q", "-am", "work"]);
        assert_eq!(worktree_status_now(repo.path(), &wt).unwrap().ahead, 1);
        git_in(repo.path(), &["merge", "--ff-only", "card/a1b2c3"]);
        assert_eq!(
            teardown_worktree(repo.path(), &wt, false),
            TeardownOutcome::Removed
        );
        assert!(!tree.exists());
        assert!(git_in(repo.path(), &["branch", "--list", "card/a1b2c3"]).is_empty());
    }

    #[test]
    fn teardown_keeps_a_dirty_tree() {
        let Some(repo) = git_repo() else { return };
        let card = Card::new("a1b2c3".into(), "t".into(), None, now_stamp());
        let BringUp::Worktree(wt) = bring_up_worktree(repo.path(), &card).unwrap() else {
            panic!()
        };
        let tree = std::path::Path::new(&wt.path);
        std::fs::write(tree.join("f.txt"), "two\n").unwrap();
        assert_eq!(
            teardown_worktree(repo.path(), &wt, false),
            TeardownOutcome::Dirty
        );
        assert!(tree.exists());
        assert!(!git_in(repo.path(), &["branch", "--list", "card/a1b2c3"]).is_empty());
        // Discard is the only forcing path
        assert_eq!(
            teardown_worktree(repo.path(), &wt, true),
            TeardownOutcome::Removed
        );
        assert!(!tree.exists());
        assert!(git_in(repo.path(), &["branch", "--list", "card/a1b2c3"]).is_empty());
    }

    #[test]
    fn teardown_keeps_an_unmerged_branch_and_reports_the_count() {
        let Some(repo) = git_repo() else { return };
        let card = Card::new("a1b2c3".into(), "t".into(), None, now_stamp());
        let BringUp::Worktree(wt) = bring_up_worktree(repo.path(), &card).unwrap() else {
            panic!()
        };
        let tree = std::path::Path::new(&wt.path);
        git_in(tree, &["commit", "-q", "--allow-empty", "-m", "one"]);
        git_in(tree, &["commit", "-q", "--allow-empty", "-m", "two"]);
        assert_eq!(
            teardown_worktree(repo.path(), &wt, false),
            TeardownOutcome::Unmerged { ahead: 2 }
        );
        assert!(!tree.exists(), "the clean tree itself is removed");
        assert!(!git_in(repo.path(), &["branch", "--list", "card/a1b2c3"]).is_empty());
        // the status probe survives a missing directory
        let st = worktree_status_now(repo.path(), &wt).unwrap();
        assert!(st.missing);
        assert_eq!(st.ahead, 2);
    }

    /// The state a `git worktree remove` interrupted by a Windows cwd hold
    /// leaves behind: contents and registration gone, the empty top
    /// directory not, the branch untouched.
    fn strand_empty_dir(repo: &std::path::Path, wt: &Worktree) {
        git_in(repo, &["worktree", "remove", "--force", &wt.path]);
        std::fs::create_dir(&wt.path).unwrap();
        assert!(
            !git_in(repo, &["worktree", "list", "--porcelain"]).contains(&wt.path),
            "precondition: the directory is unregistered"
        );
    }

    #[test]
    fn teardown_removes_an_empty_unregistered_leftover_and_the_merged_branch() {
        let Some(repo) = git_repo() else { return };
        let card = Card::new("a1b2c3".into(), "t".into(), None, now_stamp());
        let BringUp::Worktree(wt) = bring_up_worktree(repo.path(), &card).unwrap() else {
            panic!()
        };
        let tree = std::path::Path::new(&wt.path);
        std::fs::write(tree.join("f.txt"), "two\n").unwrap();
        git_in(tree, &["commit", "-q", "-am", "work"]);
        git_in(repo.path(), &["merge", "--ff-only", "card/a1b2c3"]);
        strand_empty_dir(repo.path(), &wt);
        // the probe must not read the main checkout's status as the card's
        assert!(worktree_status_now(repo.path(), &wt).unwrap().missing);
        assert_eq!(
            teardown_worktree(repo.path(), &wt, false),
            TeardownOutcome::Removed
        );
        assert!(!tree.exists());
        assert!(git_in(repo.path(), &["branch", "--list", "card/a1b2c3"]).is_empty());
    }

    #[test]
    fn teardown_with_nothing_left_is_a_completed_cleanup() {
        // Missing directory, unregistered, branch already deleted: a second
        // run after a successful first one, or after cleanup by hand.
        let Some(repo) = git_repo() else { return };
        let card = Card::new("a1b2c3".into(), "t".into(), None, now_stamp());
        let BringUp::Worktree(wt) = bring_up_worktree(repo.path(), &card).unwrap() else {
            panic!()
        };
        assert_eq!(
            teardown_worktree(repo.path(), &wt, false),
            TeardownOutcome::Removed
        );
        assert_eq!(
            teardown_worktree(repo.path(), &wt, false),
            TeardownOutcome::Removed,
            "repeating a finished teardown is not an error"
        );
        assert_eq!(
            teardown_worktree(repo.path(), &wt, true),
            TeardownOutcome::Removed
        );
    }

    #[test]
    fn teardown_of_an_empty_leftover_still_keeps_an_unmerged_branch() {
        let Some(repo) = git_repo() else { return };
        let card = Card::new("a1b2c3".into(), "t".into(), None, now_stamp());
        let BringUp::Worktree(wt) = bring_up_worktree(repo.path(), &card).unwrap() else {
            panic!()
        };
        let tree = std::path::Path::new(&wt.path);
        git_in(tree, &["commit", "-q", "--allow-empty", "-m", "one"]);
        strand_empty_dir(repo.path(), &wt);
        assert_eq!(
            teardown_worktree(repo.path(), &wt, false),
            TeardownOutcome::Unmerged { ahead: 1 }
        );
        assert!(!tree.exists(), "the empty leftover is gone");
        assert!(!git_in(repo.path(), &["branch", "--list", "card/a1b2c3"]).is_empty());
        // and the branch-only state is retry-safe too
        assert_eq!(
            teardown_worktree(repo.path(), &wt, false),
            TeardownOutcome::Unmerged { ahead: 1 }
        );
        assert_eq!(
            teardown_worktree(repo.path(), &wt, true),
            TeardownOutcome::Removed
        );
        assert!(git_in(repo.path(), &["branch", "--list", "card/a1b2c3"]).is_empty());
    }

    #[test]
    fn teardown_keeps_a_nonempty_unregistered_directory_and_its_branch() {
        // Git no longer tracks it, so nothing inside is ours to delete —
        // not even under --force.
        let Some(repo) = git_repo() else { return };
        let card = Card::new("a1b2c3".into(), "t".into(), None, now_stamp());
        let BringUp::Worktree(wt) = bring_up_worktree(repo.path(), &card).unwrap() else {
            panic!()
        };
        let tree = std::path::Path::new(&wt.path);
        strand_empty_dir(repo.path(), &wt);
        std::fs::write(tree.join("notes.txt"), "keep me\n").unwrap();
        for force in [false, true] {
            match teardown_worktree(repo.path(), &wt, force) {
                TeardownOutcome::Failed(e) => {
                    assert!(e.contains("not empty"), "actionable message, got: {e}");
                    assert!(e.contains(&wt.path), "names the directory, got: {e}");
                }
                other => panic!("expected Failed, got {other:?}"),
            }
            assert!(tree.join("notes.txt").exists(), "contents preserved");
            assert!(
                !git_in(repo.path(), &["branch", "--list", "card/a1b2c3"]).is_empty(),
                "the branch step does not run after a kept directory"
            );
        }
    }

    #[test]
    fn teardown_never_removes_an_empty_directory_outside_the_worktrees_dir() {
        // A card file edited by hand must not point teardown at an arbitrary
        // directory, even an empty one.
        let Some(repo) = git_repo() else { return };
        let stray = repo.path().join("stray");
        std::fs::create_dir(&stray).unwrap();
        let wt = Worktree {
            path: stray.to_string_lossy().replace('\\', "/"),
            branch: "card/a1b2c3".into(),
            base: "main".into(),
            in_place: false,
        };
        assert!(matches!(
            teardown_worktree(repo.path(), &wt, true),
            TeardownOutcome::Failed(_)
        ));
        assert!(stray.is_dir(), "kept");
    }

    #[test]
    fn trailer_commits_reads_real_trailers_oldest_first() {
        let Some(repo) = git_repo() else { return };
        git_in(
            repo.path(),
            &["commit", "-q", "--allow-empty", "-m", "one\n\nCard: a1b2c3"],
        );
        let one = git_in(repo.path(), &["rev-parse", "--short", "HEAD"]);
        git_in(
            repo.path(),
            &["commit", "-q", "--allow-empty", "-m", "two, no trailer"],
        );
        git_in(
            repo.path(),
            &[
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "three\n\nCard: a1b2c3\nCard: d4e5f6",
            ],
        );
        let three = git_in(repo.path(), &["rev-parse", "--short", "HEAD"]);
        let m = trailer_commits(repo.path(), "2000-01-01T00:00:00Z");
        assert_eq!(m.get("a1b2c3").unwrap(), &vec![one, three.clone()]);
        assert_eq!(m.get("d4e5f6").unwrap(), &vec![three]);
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn trailer_commits_and_latest_tag_fail_open_outside_a_repo() {
        if !git_available() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        assert!(trailer_commits(tmp.path(), "2000-01-01T00:00:00Z").is_empty());
        assert_eq!(latest_v_tag(tmp.path()), None);
    }

    #[test]
    fn latest_v_tag_picks_the_highest_version() {
        let Some(repo) = git_repo() else { return };
        assert_eq!(latest_v_tag(repo.path()), None);
        git_in(repo.path(), &["tag", "v0.9.0"]);
        git_in(repo.path(), &["tag", "v0.10.0"]);
        git_in(repo.path(), &["tag", "release-1"]);
        assert_eq!(latest_v_tag(repo.path()).as_deref(), Some("v0.10.0"));
    }

    // --- dispatch-worktrees: schema, naming, status, list lines ---

    #[test]
    fn dispatch_prompt_with_worktree_renders_workspace_and_integration_lines() {
        let mut card = sample_card(Some("Resize flickers on Up-arrow."));
        card.worktree = Some(sample_worktree());
        let s = dispatch_prompt(&card, CloseoutStyle::Path);
        assert_eq!(
            s,
            "You are a worker Session dispatched from card a3f8k2 on this project's board.\n\
             \n\
             # Task: Fix resize flicker\n\
             \n\
             Resize flickers on Up-arrow.\n\
             \n\
             # Workspace\n\
             You are in a git worktree at H:/repo/.foreman/worktrees/a3f8k2, on branch card/a3f8k2, based on main.\n\
             The main checkout at H:/repo is shared with other workers: never edit files there.\n\
             Leave .foreman/ untouched and never stage it.\n\
             \n\
             # Close-out (required)\n\
             End every commit message with the trailer line:    Card: a3f8k2\n\
             Commit everything in your worktree, then hand integration to Foreman's queue (never merge into the main checkout yourself):\n\
             \x20   foreman kanban integrate a3f8k2\n\
             \x20   foreman kanban wait a3f8k2 --timeout 1800\n\
             wait exit 0: Foreman rebased onto main, ran the project checks, fast-forwarded main, and marked the card Done. You are finished; do not run done yourself.\n\
             wait exit 3: the rebase conflicted or a check failed. Read the reason with foreman kanban list --json (the \"integration\" object), fix it in your worktree (finish the rebase, commit), then run integrate and wait again. Queued is not Done.\n\
             wait exit 2: still queued or checking; run wait again.\n\
             If you are stuck and need a human: foreman kanban block a3f8k2 --reason \"<one line>\"\n\
             Do not end the session without the card Done (by the queue) or blocked."
        );
        assert!(
            !s.contains("merge --ff-only") && !s.contains("kanban done"),
            "a worktree worker never merges or marks done itself: {s}"
        );
    }

    #[test]
    fn dispatch_prompt_with_worktree_envvar_style_keeps_git_lines_style_independent() {
        let mut card = sample_card(None);
        card.worktree = Some(sample_worktree());
        let s = dispatch_prompt(&card, CloseoutStyle::EnvVar);
        assert!(s.contains("# Workspace\n"));
        assert!(s.contains(
            "    & $env:FOREMAN_EXE kanban integrate a3f8k2\n    & $env:FOREMAN_EXE kanban wait a3f8k2 --timeout 1800\n"
        ));
        assert!(s.contains("Read the reason with & $env:FOREMAN_EXE kanban list --json"));
        assert!(s.contains(
            "If you are stuck and need a human: & $env:FOREMAN_EXE kanban block a3f8k2 --reason \"<one line>\"\n"
        ));
        assert!(s.contains("(bash: write \"$FOREMAN_EXE\" in place of & $env:FOREMAN_EXE)\n"));
        assert!(!s.contains("kanban done"));
    }

    #[test]
    fn same_path_follows_the_directory_when_string_forms_differ() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("wt");
        std::fs::create_dir(&dir).unwrap();
        let raw = dir.to_string_lossy().replace('\\', "/");
        let canon = std::fs::canonicalize(&dir).unwrap();
        let canon_s = canon.to_string_lossy();
        assert!(same_path(&raw, &canon_s), "raw={raw:?} canon={canon_s:?}");
        assert!(same_path(&raw, &raw));
        assert!(
            !same_path(&raw, &format!("{raw}-nope")),
            "a missing sibling is not the same place"
        );
    }

    #[test]
    fn worktree_layout_names_path_and_branch_with_forward_slashes() {
        let wt = worktree_layout(
            std::path::Path::new(r"H:\claude code\foreman"),
            "etxvs5",
            "main",
        );
        assert_eq!(wt.path, "H:/claude code/foreman/.foreman/worktrees/etxvs5");
        assert_eq!(wt.branch, "card/etxvs5");
        assert_eq!(wt.base, "main");
        assert_eq!(
            wt.root(),
            std::path::PathBuf::from("H:/claude code/foreman")
        );
        // a trailing slash on root does not double up
        let wt = worktree_layout(std::path::Path::new("C:/repo/"), "a1b2c3", "dev");
        assert_eq!(wt.path, "C:/repo/.foreman/worktrees/a1b2c3");
    }

    #[test]
    fn parse_status_reads_porcelain_and_left_right_counts() {
        let s = parse_status(Some(""), "0\t0");
        assert_eq!(s, WorktreeStatus::default());
        let s = parse_status(Some(" M src/wm.rs\n"), "2\t3");
        assert_eq!(
            s,
            WorktreeStatus {
                dirty: true,
                ahead: 3,
                behind: 2,
                missing: false
            }
        );
        let s = parse_status(None, "1\t0");
        assert!(s.missing);
        assert!(!s.dirty);
        assert_eq!((s.behind, s.ahead), (1, 0));
        // garbage rev-list output degrades to zeros, never a panic
        let s = parse_status(Some(""), "fatal: bad revision");
        assert_eq!((s.behind, s.ahead), (0, 0));
    }

    fn sample_worktree() -> Worktree {
        Worktree {
            path: "H:/repo/.foreman/worktrees/a3f8k2".into(),
            branch: "card/a3f8k2".into(),
            base: "main".into(),
            in_place: false,
        }
    }

    #[test]
    fn worktree_summary_renders_branch_counts_and_flags() {
        let wt = sample_worktree();
        assert_eq!(worktree_summary(&wt, None), "card/a3f8k2");
        let st = WorktreeStatus {
            dirty: false,
            ahead: 3,
            behind: 1,
            missing: false,
        };
        assert_eq!(worktree_summary(&wt, Some(&st)), "card/a3f8k2 +3 -1");
        let st = WorktreeStatus { dirty: true, ..st };
        assert_eq!(worktree_summary(&wt, Some(&st)), "card/a3f8k2 +3 -1 dirty");
        let st = WorktreeStatus {
            missing: true,
            ..Default::default()
        };
        assert_eq!(
            worktree_summary(&wt, Some(&st)),
            "card/a3f8k2 +0 -0 missing"
        );
    }

    #[test]
    fn v1_card_file_without_worktree_round_trips_unchanged() {
        let j = r#"{"v":1,"id":"a3f8k2","title":"t","state":"backlog","created":"2026-08-28T13:55:00Z","updated":"2026-08-28T13:55:00Z"}"#;
        let c: Card = serde_json::from_str(j).unwrap();
        assert!(c.worktree.is_none());
        assert_eq!(serde_json::to_string(&c).unwrap(), j);
    }

    fn sample_shipped() -> Shipped {
        Shipped {
            name: "v0.4.9".into(),
            at: "2026-09-16T23:10:00Z".into(),
            commits: vec!["0ea479a".into(), "a52089e".into()],
        }
    }

    #[test]
    fn shipped_card_json_round_trips_the_object_and_omits_empty_commits() {
        let mut c = sample_card(None);
        c.state = CardState::Done;
        c.shipped = Some(sample_shipped());
        let s = serde_json::to_string(&c).unwrap();
        assert!(s.contains(
            r#""shipped":{"name":"v0.4.9","at":"2026-09-16T23:10:00Z","commits":["0ea479a","a52089e"]}"#
        ));
        let back: Card = serde_json::from_str(&s).unwrap();
        assert_eq!(back, c);

        c.shipped.as_mut().unwrap().commits.clear();
        let s = serde_json::to_string(&c).unwrap();
        assert!(!s.contains("commits"), "{s}");
        let back: Card = serde_json::from_str(&s).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn v1_card_file_without_shipped_round_trips_unchanged() {
        let j = r#"{"v":1,"id":"a3f8k2","title":"t","state":"done","created":"2026-08-28T13:55:00Z","updated":"2026-08-28T13:55:00Z"}"#;
        let c: Card = serde_json::from_str(j).unwrap();
        assert!(c.shipped.is_none());
        assert_eq!(serde_json::to_string(&c).unwrap(), j);
    }

    #[test]
    fn shipped_with_an_empty_name_loads_as_unshipped() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".foreman").join("tasks");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("zz0001.json"),
            r#"{"v":1,"id":"zz0001","title":"t","state":"done","shipped":{"name":"  ","at":"2026-09-16T23:10:00Z"},"created":"2026-08-28T13:55:00Z","updated":"2026-08-28T13:55:00Z"}"#,
        )
        .unwrap();
        let mut s = store_at(tmp.path());
        s.reload();
        assert!(s.get("zz0001").unwrap().shipped.is_none());
    }

    #[test]
    fn same_name_folds_case_and_whitespace() {
        assert!(same_name("v1", "V1"));
        assert!(same_name(" v1 ", "v1"));
        assert!(same_name("current", CURRENT));
        assert!(!same_name("v1", "v10"));
    }

    #[test]
    fn versions_are_distinct_case_insensitive_and_newest_first() {
        let mut a = sample_card(None);
        a.id = "a".into();
        a.shipped = Some(Shipped {
            name: "v1".into(),
            at: "2026-09-01T00:00:00Z".into(),
            commits: vec![],
        });
        let mut b = a.clone();
        b.id = "b".into();
        b.shipped.as_mut().unwrap().name = "V1".into();
        let mut c = a.clone();
        c.id = "c".into();
        c.shipped = Some(Shipped {
            name: "v2".into(),
            at: "2026-09-10T00:00:00Z".into(),
            commits: vec![],
        });
        let mut d = a.clone();
        d.id = "d".into();
        d.shipped = None;
        let v = versions(&[a, b, c, d]);
        assert_eq!(
            v,
            vec![
                Version {
                    name: "v2".into(),
                    at: "2026-09-10T00:00:00Z".into(),
                    count: 1
                },
                Version {
                    name: "v1".into(),
                    at: "2026-09-01T00:00:00Z".into(),
                    count: 2
                },
            ]
        );
    }

    #[test]
    fn v1_card_file_without_planned_round_trips_unchanged() {
        let j = r#"{"v":1,"id":"a3f8k2","title":"t","state":"backlog","created":"2026-08-28T13:55:00Z","updated":"2026-08-28T13:55:00Z"}"#;
        let c: Card = serde_json::from_str(j).unwrap();
        assert!(c.planned.is_none());
        assert_eq!(serde_json::to_string(&c).unwrap(), j);
    }

    #[test]
    fn planned_card_json_round_trips_and_a_waveless_file_reads_as_wave_one() {
        let mut c = sample_card(None);
        c.planned = Some(Planned {
            name: "Terminal work".into(),
            wave: 2,
        });
        let s = serde_json::to_string(&c).unwrap();
        assert!(
            s.contains(r#""planned":{"name":"Terminal work","wave":2}"#),
            "{s}"
        );
        assert_eq!(serde_json::from_str::<Card>(&s).unwrap(), c);

        let j = r#"{"v":1,"id":"a3f8k2","title":"t","state":"backlog","planned":{"name":"Terminal work"},"created":"2026-08-28T13:55:00Z","updated":"2026-08-28T13:55:00Z"}"#;
        let back: Card = serde_json::from_str(j).unwrap();
        assert_eq!(back.planned.unwrap().wave, 1);
    }

    #[test]
    fn planned_with_an_empty_name_loads_as_unplanned() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".foreman").join("tasks");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("zz0002.json"),
            r#"{"v":1,"id":"zz0002","title":"t","state":"backlog","planned":{"name":"  ","wave":3},"created":"2026-08-28T13:55:00Z","updated":"2026-08-28T13:55:00Z"}"#,
        )
        .unwrap();
        let mut s = store_at(tmp.path());
        s.reload();
        assert!(s.get("zz0002").unwrap().planned.is_none());
    }

    fn planned_card(id: &str, name: &str, wave: u32, state: CardState, created: &str) -> Card {
        let mut c = sample_card(None);
        c.id = id.into();
        c.title = format!("title {id}");
        c.state = state;
        c.created = created.into();
        c.planned = Some(Planned {
            name: name.into(),
            wave,
        });
        c
    }

    #[test]
    fn plans_fold_names_case_insensitively_and_order_waves_ascending() {
        let cards = vec![
            planned_card(
                "a",
                "Terminal work",
                2,
                CardState::Backlog,
                "2026-09-01T00:00:00Z",
            ),
            planned_card(
                "b",
                "terminal WORK",
                1,
                CardState::Done,
                "2026-09-02T00:00:00Z",
            ),
            planned_card("c", "Chat", 1, CardState::Backlog, "2026-09-05T00:00:00Z"),
            sample_card(None),
        ];
        let p = plans(&cards);
        // "Chat" has the newest card, so it sorts first; the folded plan is
        // spelled the way it was first seen.
        assert_eq!(
            p.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
            vec!["Chat", "Terminal work"]
        );
        let tw = &p[1];
        assert_eq!(tw.at, "2026-09-02T00:00:00Z");
        assert_eq!(
            tw.waves.iter().map(|w| w.number).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(tw.waves[0].cards[0].id, "b");
        assert_eq!(tw.waves[1].cards[0].title, "title a");
        assert_eq!(tw.card_count(), 2);
    }

    #[test]
    fn current_wave_is_the_lowest_holding_a_non_done_card() {
        let done_first = vec![
            planned_card("a", "P", 1, CardState::Done, "2026-09-01T00:00:00Z"),
            planned_card("b", "P", 2, CardState::Done, "2026-09-02T00:00:00Z"),
            planned_card("c", "P", 2, CardState::InProgress, "2026-09-03T00:00:00Z"),
            planned_card("d", "P", 5, CardState::Backlog, "2026-09-04T00:00:00Z"),
        ];
        assert_eq!(plans(&done_first)[0].current(), Some(2));

        // Blocked is not Done, so it still holds its wave.
        let blocked = vec![planned_card(
            "a",
            "P",
            7,
            CardState::Blocked,
            "2026-09-01T00:00:00Z",
        )];
        assert_eq!(plans(&blocked)[0].current(), Some(7));

        // Every card Done = finished, not "stuck on the last wave".
        let all_done = vec![planned_card(
            "a",
            "P",
            1,
            CardState::Done,
            "2026-09-01T00:00:00Z",
        )];
        assert_eq!(plans(&all_done)[0].current(), None);

        assert!(plans(&[sample_card(None)]).is_empty());
    }

    #[test]
    fn edit_sets_clears_and_renumbers_a_plan() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_at(tmp.path());
        let id = store.add("card", None).unwrap();

        store
            .edit(&id, None, None, Some("  Terminal work  "), None)
            .unwrap();
        let pl = store.get(&id).unwrap().planned.clone().unwrap();
        assert_eq!(pl.name, "Terminal work", "the name is stored trimmed");
        assert_eq!(pl.wave, 1, "--plan alone starts at wave 1");

        store.edit(&id, None, None, None, Some(3)).unwrap();
        assert_eq!(store.get(&id).unwrap().planned.as_ref().unwrap().wave, 3);
        assert_eq!(
            store.get(&id).unwrap().planned.as_ref().unwrap().name,
            "Terminal work",
            "--wave alone keeps the plan"
        );

        store.edit(&id, Some("renamed"), None, None, None).unwrap();
        assert!(
            store.get(&id).unwrap().planned.is_some(),
            "editing the title leaves the plan alone"
        );

        store.edit(&id, None, None, Some(""), None).unwrap();
        assert!(
            store.get(&id).unwrap().planned.is_none(),
            "--plan \"\" clears"
        );

        // A wave with no plan orders nothing, and the refusal writes nothing.
        let before = store.get(&id).unwrap().updated.clone();
        assert!(store.edit(&id, None, None, None, Some(2)).is_err());
        assert_eq!(store.get(&id).unwrap().updated, before);
        // Clearing and numbering in one call contradict each other.
        assert!(store.edit(&id, None, None, Some(""), Some(2)).is_err());
    }

    #[test]
    fn human_line_carries_the_version_tail() {
        let mut card = sample_card(None);
        card.state = CardState::Done;
        card.shipped = Some(sample_shipped());
        let line = CardLine {
            card,
            orphaned: false,
            worktree_status: None,
            integration: None,
        };
        assert_eq!(
            line.human_line(),
            "a3f8k2  done  Fix resize flicker  [shipped v0.4.9]"
        );
    }

    #[test]
    fn parse_trailer_log_buckets_by_id_oldest_first_and_skips_untagged() {
        // git log order: newest first. `ccc` names x1; `bbb` has no
        // trailer; `aaa` names x1 and x2.
        let text = "ccc\tx1\nbbb\t\naaa\tx1, x2\n";
        let m = parse_trailer_log(text);
        assert_eq!(
            m.get("x1").unwrap(),
            &vec!["aaa".to_string(), "ccc".to_string()]
        );
        assert_eq!(m.get("x2").unwrap(), &vec!["aaa".to_string()]);
        assert_eq!(m.len(), 2);
        assert!(parse_trailer_log("").is_empty());
    }

    /// Add a card, claim it as if dispatched (no live terminal needed), and
    /// close it out — the only legal road to Done.
    fn add_done(s: &mut CardStore, title: &str, wt: Option<Worktree>) -> String {
        let id = s.add(title, None).unwrap();
        s.claim_for_dispatch(&id, "t1", "claude", run_nonce(), TermState::Missing, wt)
            .unwrap();
        s.done(&id).unwrap();
        id
    }

    fn no_hold(_: &Card) -> Option<String> {
        None
    }

    fn no_commits(_: &[Card]) -> std::collections::HashMap<String, Vec<String>> {
        Default::default()
    }

    #[test]
    fn cut_stamps_every_ungrouped_done_card_and_nothing_else() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = store_at(tmp.path());
        let backlog = s.add("stays", None).unwrap();
        let a = add_done(&mut s, "a", None);
        let b = add_done(&mut s, "b", None);
        let out = s.cut(" v1 ", no_hold, no_commits).unwrap();
        assert_eq!(out.name, "v1");
        let mut shipped = out.shipped.clone();
        shipped.sort();
        let mut want = vec![a.clone(), b.clone()];
        want.sort();
        assert_eq!(shipped, want);
        assert!(out.held_back.is_empty());
        assert_eq!(out.lines(), vec!["cut v1: 2 cards".to_string()]);

        let sa = s.get(&a).unwrap().shipped.clone().unwrap();
        let sb = s.get(&b).unwrap().shipped.clone().unwrap();
        assert_eq!(sa.name, "v1");
        assert_eq!(sa.at, sb.at, "one stamp for the whole Cut");
        assert_eq!(s.get(&a).unwrap().updated, sa.at);
        assert_eq!(s.get(&a).unwrap().state, CardState::Done);
        assert!(s.get(&backlog).unwrap().shipped.is_none());
        // the files agree with memory
        let mut fresh = store_at(tmp.path());
        fresh.reload();
        assert_eq!(fresh.get(&a).unwrap().shipped, Some(sa));
    }

    #[test]
    fn cut_refuses_empty_done_blank_current_and_duplicate_names() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = store_at(tmp.path());
        let e = s.cut("v1", no_hold, no_commits).unwrap_err();
        assert!(e.contains("nothing in Done"), "{e}");
        add_done(&mut s, "a", None);
        let e = s.cut("   ", no_hold, no_commits).unwrap_err();
        assert!(e.contains("name"), "{e}");
        let e = s.cut("current", no_hold, no_commits).unwrap_err();
        assert!(e.contains("Current"), "{e}");
        s.cut("v1", no_hold, no_commits).unwrap();
        add_done(&mut s, "b", None);
        let e = s.cut("V1", no_hold, no_commits).unwrap_err();
        assert!(e.contains("already exists"), "{e}");
    }

    #[test]
    fn cut_holds_back_unmerged_worktree_cards_and_reports_them() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = store_at(tmp.path());
        let a = add_done(&mut s, "merged", None);
        let b = add_done(&mut s, "stranded", Some(sample_worktree()));
        let hold = |c: &Card| Some(format!("unmerged {}", c.worktree.as_ref().unwrap().branch));
        let out = s.cut("v1", hold, no_commits).unwrap();
        assert_eq!(out.shipped, vec![a.clone()]);
        assert_eq!(
            out.held_back,
            vec![(b.clone(), "unmerged card/a3f8k2".to_string())]
        );
        assert_eq!(
            out.lines(),
            vec![
                "cut v1: 1 cards".to_string(),
                format!("{b} stayed in Current (unmerged card/a3f8k2)"),
            ]
        );
        assert!(s.get(&b).unwrap().shipped.is_none());
        assert!(s.get(&a).unwrap().shipped.is_some());
    }

    #[test]
    fn cut_with_every_candidate_held_back_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = store_at(tmp.path());
        let b = add_done(&mut s, "stranded", Some(sample_worktree()));
        let card_path = tmp
            .path()
            .join(".foreman")
            .join("tasks")
            .join(format!("{b}.json"));
        let before = std::fs::read_to_string(&card_path).unwrap();
        let e = s
            .cut("v1", |_| Some("unmerged".into()), no_commits)
            .unwrap_err();
        assert!(e.contains("nothing to cut"), "{e}");
        let after = std::fs::read_to_string(&card_path).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn cut_attaches_commits_from_the_lookup() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = store_at(tmp.path());
        let a = add_done(&mut s, "a", None);
        let b = add_done(&mut s, "b", None);
        let a2 = a.clone();
        let commits = move |cards: &[Card]| {
            assert_eq!(cards.len(), 2, "the lookup sees the surviving candidates");
            let mut m: std::collections::HashMap<String, Vec<String>> = Default::default();
            m.insert(a2.clone(), vec!["abc1234".into(), "def5678".into()]);
            m
        };
        s.cut("v1", no_hold, commits).unwrap();
        assert_eq!(
            s.get(&a).unwrap().shipped.as_ref().unwrap().commits,
            vec!["abc1234".to_string(), "def5678".to_string()]
        );
        assert!(
            s.get(&b)
                .unwrap()
                .shipped
                .as_ref()
                .unwrap()
                .commits
                .is_empty()
        );
        let text = std::fs::read_to_string(
            tmp.path()
                .join(".foreman")
                .join("tasks")
                .join(format!("{b}.json")),
        )
        .unwrap();
        assert!(!text.contains("commits"), "{text}");
    }

    #[test]
    fn cut_reverts_when_a_write_fails_midway() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = store_at(tmp.path());
        add_done(&mut s, "a", None);
        add_done(&mut s, "b", None);
        add_done(&mut s, "c", None);
        // `write_card` stages `<id>.json.tmp`; a DIRECTORY at that path makes
        // the second candidate's write fail after the first succeeded.
        let victim = s.cards()[1].id.clone();
        std::fs::create_dir(
            tmp.path()
                .join(".foreman")
                .join("tasks")
                .join(format!("{victim}.json.tmp")),
        )
        .unwrap();
        let e = s.cut("v1", no_hold, no_commits).unwrap_err();
        assert!(e.contains("nothing shipped"), "{e}");
        assert!(e.contains(&victim), "{e}");
        let mut fresh = store_at(tmp.path());
        fresh.reload();
        assert!(
            fresh.cards().iter().all(|c| c.shipped.is_none()),
            "no card may be left stamped"
        );
        assert!(s.cards().iter().all(|c| c.shipped.is_none()));
    }

    #[test]
    fn cut_reloads_before_judging() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = store_at(tmp.path());
        let a = add_done(&mut s, "a", None);
        // Dropped on disk behind the store's back (a pull, another writer).
        let dir = tmp.path().join(".foreman").join("tasks");
        std::fs::write(
            dir.join("zz0002.json"),
            r#"{"v":1,"id":"zz0002","title":"t","state":"done","created":"2026-08-28T13:55:00Z","updated":"2026-08-28T13:55:00Z"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("zz0003.json"),
            r#"{"v":1,"id":"zz0003","title":"t","state":"done","shipped":{"name":"v2","at":"2026-09-01T00:00:00Z"},"created":"2026-08-28T13:55:00Z","updated":"2026-08-28T13:55:00Z"}"#,
        )
        .unwrap();
        let e = s.cut("V2", no_hold, no_commits).unwrap_err();
        assert!(e.contains("already exists"), "{e}");
        let out = s.cut("v3", no_hold, no_commits).unwrap();
        let mut got = out.shipped.clone();
        got.sort();
        let mut want = vec![a, "zz0002".to_string()];
        want.sort();
        assert_eq!(got, want);
    }

    #[test]
    fn uncut_clears_the_version_case_insensitively_and_refuses_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = store_at(tmp.path());
        let a = add_done(&mut s, "a", None);
        let b = add_done(&mut s, "b", None);
        s.cut("v1", no_hold, no_commits).unwrap();
        let at = s.get(&a).unwrap().shipped.as_ref().unwrap().at.clone();
        assert_eq!(s.uncut("V1").unwrap(), 2);
        assert!(s.get(&a).unwrap().shipped.is_none());
        assert!(s.get(&b).unwrap().shipped.is_none());
        assert!(s.get(&a).unwrap().updated >= at);
        assert_eq!(s.get(&a).unwrap().state, CardState::Done);
        let e = s.uncut("v1").unwrap_err();
        assert!(e.contains("no version"), "{e}");
    }

    #[test]
    fn already_shipped_cards_survive_a_later_cut() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = store_at(tmp.path());
        let a = add_done(&mut s, "a", None);
        s.cut("v1", no_hold, no_commits).unwrap();
        let c = add_done(&mut s, "c", None);
        let out = s.cut("v2", no_hold, no_commits).unwrap();
        assert_eq!(out.shipped, vec![c.clone()]);
        assert_eq!(s.get(&a).unwrap().shipped.as_ref().unwrap().name, "v1");
        assert_eq!(s.get(&c).unwrap().shipped.as_ref().unwrap().name, "v2");
    }

    #[test]
    fn clear_worktree_on_a_shipped_card_keeps_shipped() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = store_at(tmp.path());
        let b = add_done(&mut s, "b", Some(sample_worktree()));
        s.cut("v1", no_hold, no_commits).unwrap();
        let before = s.get(&b).unwrap().shipped.clone().unwrap();
        s.clear_worktree(&b).unwrap();
        let card = s.get(&b).unwrap();
        assert!(card.worktree.is_none());
        assert_eq!(card.shipped.as_ref(), Some(&before));
    }

    #[test]
    fn card_with_worktree_serializes_the_spec_object() {
        let mut c = sample_card(None);
        c.worktree = Some(sample_worktree());
        let s = serde_json::to_string(&c).unwrap();
        assert!(
            s.contains(
                r#""worktree":{"path":"H:/repo/.foreman/worktrees/a3f8k2","branch":"card/a3f8k2","base":"main"}"#
            ),
            "{s}"
        );
        let back: Card = serde_json::from_str(&s).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn claim_for_dispatch_records_and_keeps_the_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_at(tmp.path());
        let run = run_nonce();
        let id = store.add("card", None).unwrap();
        store
            .claim_for_dispatch(
                &id,
                "t1",
                "claude",
                run,
                TermState::Missing,
                Some(sample_worktree()),
            )
            .unwrap();
        assert_eq!(store.get(&id).unwrap().worktree, Some(sample_worktree()));
        // done keeps the field: only teardown clears it
        store.done(&id).unwrap();
        assert_eq!(store.get(&id).unwrap().worktree, Some(sample_worktree()));
        // a later claim with None leaves it alone (Restart reuses the tree)
        let id2 = store.add("card two", None).unwrap();
        store
            .claim_for_dispatch(
                &id2,
                "t1",
                "claude",
                run,
                TermState::Missing,
                Some(sample_worktree()),
            )
            .unwrap();
        store.block(&id2, "reason").unwrap();
        store.start(&id2, "t2", run, TermState::Missing).unwrap();
        assert_eq!(store.get(&id2).unwrap().worktree, Some(sample_worktree()));
        store.clear_worktree(&id2).unwrap();
        assert!(store.get(&id2).unwrap().worktree.is_none());
        assert!(store.clear_worktree("nope00").is_err());
    }

    #[test]
    fn status_poll_batches_only_worktree_cards_when_due_and_shown() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_at(tmp.path());
        let run = run_nonce();
        let plain = store.add("plain", None).unwrap();
        let with = store.add("with", None).unwrap();
        store
            .claim_for_dispatch(
                &with,
                "t1",
                "claude",
                run,
                TermState::Missing,
                Some(sample_worktree()),
            )
            .unwrap();
        let t0 = std::time::Instant::now();
        // hidden board: nothing
        assert!(store.take_status_poll(t0).is_none());
        store.mark_shown(t0);
        let batch = store.take_status_poll(t0).unwrap();
        assert_eq!(batch, vec![(with.clone(), sample_worktree())]);
        assert!(!batch.iter().any(|(id, _)| id == &plain));
        // in flight: no second batch until results land
        assert!(
            store
                .take_status_poll(t0 + STATUS_POLL_INTERVAL * 2)
                .is_none()
        );
        let mut map = std::collections::HashMap::new();
        map.insert(
            with.clone(),
            WorktreeStatus {
                ahead: 2,
                ..Default::default()
            },
        );
        store.set_worktree_statuses(map, Vec::new());
        assert_eq!(store.worktree_status(&with).unwrap().ahead, 2);
        // interval not elapsed since the last kick
        assert!(
            store
                .take_status_poll(t0 + std::time::Duration::from_secs(1))
                .is_none()
        );
        store.mark_shown(t0 + STATUS_POLL_INTERVAL * 2);
        assert!(
            store
                .take_status_poll(t0 + STATUS_POLL_INTERVAL * 2)
                .is_some()
        );
        // A round that started before a teardown cleared a card's field
        // must not resurrect a status for it: `list --json` would then
        // print `worktree_status` with no `worktree`.
        store.clear_worktree(&with).unwrap();
        let mut stale = std::collections::HashMap::new();
        stale.insert(with.clone(), WorktreeStatus::default());
        stale.insert(plain.clone(), WorktreeStatus::default());
        store.set_worktree_statuses(stale, Vec::new());
        assert!(store.worktree_status(&with).is_none());
        assert!(store.worktree_status(&plain).is_none());
    }

    #[test]
    fn card_line_carries_worktree_fields_only_when_present() {
        let line = CardLine {
            card: sample_card(None),
            orphaned: false,
            worktree_status: None,
            integration: None,
        };
        let j = line.json_line();
        assert!(!j.contains("worktree"), "{j}");
        let mut card = sample_card(None);
        card.worktree = Some(sample_worktree());
        let st = WorktreeStatus {
            dirty: true,
            ahead: 3,
            behind: 1,
            missing: false,
        };
        let line = CardLine {
            card,
            orphaned: false,
            worktree_status: Some(st),
            integration: None,
        };
        let j = line.json_line();
        assert!(j.contains(r#""worktree":{"#), "{j}");
        assert!(
            j.contains(r#""worktree_status":{"dirty":true,"ahead":3,"behind":1,"missing":false}"#),
            "{j}"
        );
        let back: CardLine = serde_json::from_str(&j).unwrap();
        assert_eq!(back, line);
        assert_eq!(
            line.human_line(),
            "a3f8k2  backlog  Fix resize flicker  [wt card/a3f8k2 +3 -1 dirty]"
        );
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

    fn line_integrating(id: &str, phase: crate::integrate::Phase) -> CardLine {
        let mut line = line_with_state(id, CardState::InProgress, false);
        line.integration = Some(crate::integrate::IntegrationView {
            phase,
            commit: "c".into(),
            position: None,
            stage: None,
            reason: None,
            detail: None,
            next: None,
            hold: None,
            prepared: None,
            checks: None,
            note: None,
            cancel_requested: false,
        });
        line
    }

    #[test]
    fn wait_verdict_surfaces_a_handed_back_integration_only_to_the_cards_own_waiter() {
        use crate::integrate::Phase;
        let target = WaitTarget::Id("a1".into());
        let mut watched = std::collections::HashSet::new();
        // queued / integrating: still pending
        for phase in [Phase::Queued, Phase::Integrating, Phase::Integrated] {
            let cards = vec![line_integrating("a1", phase)];
            assert_eq!(
                wait_verdict(&target, &mut watched, &cards),
                None,
                "{phase:?}"
            );
        }
        let cards = vec![line_integrating("a1", Phase::NeedsResolution)];
        assert_eq!(
            wait_verdict(&target, &mut watched, &cards),
            Some(WAIT_NEEDS_RESOLUTION)
        );
        // an orphaned worker outranks its handed-back integration
        let mut orphaned = line_integrating("a1", Phase::NeedsResolution);
        orphaned.orphaned = true;
        assert_eq!(wait_verdict(&target, &mut watched, &[orphaned]), Some(1));
        // --any leaves resolution to the live worker
        let any = WaitTarget::Any;
        let mut watched = std::collections::HashSet::new();
        let cards = vec![line_integrating("a1", Phase::NeedsResolution)];
        assert_eq!(wait_verdict(&any, &mut watched, &cards), None);
        assert!(watched.contains("a1"));
    }

    #[test]
    fn card_line_carries_the_integration_view_only_when_present() {
        let line = line_with_state("a1", CardState::InProgress, false);
        assert!(!line.json_line().contains("integration"));
        let line = line_integrating("a1", crate::integrate::Phase::Queued);
        let j = line.json_line();
        assert!(
            j.contains(r#""integration":{"phase":"queued","commit":"c"}"#),
            "{j}"
        );
        let back: CardLine = serde_json::from_str(&j).unwrap();
        assert_eq!(back, line);
        assert!(
            line.human_line().ends_with("[integrate queued]"),
            "{}",
            line.human_line()
        );
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

    const PORCELAIN: &str = "worktree H:/repo\n\
        HEAD 2494df8538bf5eb006053ceb6b191bd5639202d5\n\
        branch refs/heads/main\n\
        \n\
        worktree H:/repo/.foreman/worktrees/plds8v\n\
        HEAD a52089e76088264c03b9d793fd9d5765b4690ef3\n\
        branch refs/heads/card/plds8v\n\
        \n\
        worktree H:/repo/.foreman/worktrees/gone11\n\
        HEAD a52089e76088264c03b9d793fd9d5765b4690ef3\n\
        detached\n\
        prunable gitdir file points to non-existent location\n\
        \n\
        worktree H:/elsewhere/spike\n\
        HEAD 3cabee1e4dcdf93c05657e4b50f5513f8e019d69\n\
        branch refs/heads/spike/wgpu\n";

    #[test]
    fn parse_worktree_list_keeps_only_foreman_trees() {
        let got = parse_worktree_list(PORCELAIN, "H:/repo");
        assert_eq!(
            got,
            vec![
                (
                    "H:/repo/.foreman/worktrees/plds8v".to_string(),
                    "card/plds8v".to_string()
                ),
                (
                    "H:/repo/.foreman/worktrees/gone11".to_string(),
                    "HEAD".to_string()
                ),
            ],
            "main checkout and hand-made trees are dropped; detached reads as HEAD; prunable kept"
        );
        // Backslash root and trailing slash normalise the same way.
        assert_eq!(parse_worktree_list(PORCELAIN, "H:\\repo\\").len(), 2);
        // CRLF output (git on Windows via some shells) parses identically.
        assert_eq!(
            parse_worktree_list(&PORCELAIN.replace('\n', "\r\n"), "H:/repo").len(),
            2
        );
        assert!(parse_worktree_list("", "H:/repo").is_empty());
    }

    #[test]
    fn strays_are_the_listed_trees_no_card_points_at() {
        let owned = vec![("a3f8k2".to_string(), sample_worktree())];
        let mut other = sample_worktree();
        other.path = "H:/repo/.foreman/worktrees/zz9".into();
        other.branch = "card/zz9".into();
        // Case-insensitive on Windows, like `same_path`.
        let mut cased = sample_worktree();
        cased.path = "h:/REPO/.foreman/worktrees/A3F8K2".into();
        let listed = vec![sample_worktree(), other.clone(), cased.clone()];
        let got = strays_among(listed, &owned);
        if cfg!(windows) {
            assert_eq!(got, vec![other]);
        } else {
            assert_eq!(got, vec![other, cased]);
        }
        assert_eq!(worktree_dir_name(&sample_worktree()), "a3f8k2");
    }

    #[test]
    fn worktree_rows_order_cards_by_column_then_strays_and_apply_the_attention_rule() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_at(tmp.path());
        let run = run_nonce();
        let mut wt_for = |id: &str| Worktree {
            path: format!("H:/repo/.foreman/worktrees/{id}"),
            branch: format!("card/{id}"),
            base: "main".into(),
            in_place: false,
        };
        let done = store.add("done card", None).unwrap();
        let live = store.add("live card", None).unwrap();
        let plain = store.add("no worktree", None).unwrap();
        for id in [&done, &live] {
            store
                .claim_for_dispatch(
                    id,
                    "t1",
                    "claude",
                    run.clone(),
                    TermState::Missing,
                    Some(wt_for(id)),
                )
                .unwrap();
        }
        store.done(&done).unwrap();
        let orphan = store.add("orphan card", None).unwrap();
        store
            .claim_for_dispatch(
                &orphan,
                "t2",
                "claude",
                run.clone(),
                TermState::Missing,
                Some(wt_for(&orphan)),
            )
            .unwrap();
        let mut orphans = std::collections::HashSet::new();
        orphans.insert(orphan.clone());
        let status = |id: &str| -> Option<WorktreeStatus> {
            if id == done {
                Some(WorktreeStatus {
                    ahead: 2,
                    ..Default::default()
                })
            } else if id == live {
                Some(WorktreeStatus {
                    ahead: 3,
                    ..Default::default()
                })
            } else {
                None
            }
        };
        let strays = vec![
            StrayWorktree {
                wt: wt_for("zzz999"),
                status: Some(WorktreeStatus {
                    dirty: true,
                    ..Default::default()
                }),
            },
            StrayWorktree {
                wt: wt_for("aaa111"),
                status: None,
            },
        ];
        let rows = worktree_rows(store.cards(), &orphans, status, &strays);
        let names: Vec<String> = rows.iter().map(WorktreeRow::name).collect();
        // The two In Progress cards keep the store's order (created, then
        // id — same second here, so by id); Done follows; strays last by
        // path; no row for the card without a worktree.
        let mut in_progress = vec![live.clone(), orphan.clone()];
        in_progress.sort();
        assert_eq!(
            names,
            vec![
                in_progress[0].clone(),
                in_progress[1].clone(),
                done.clone(),
                "aaa111".into(),
                "zzz999".into()
            ],
            "no row for {plain}"
        );
        let by_name = |n: &str| rows.iter().find(|r| r.name() == n).unwrap();
        // Live claim: Open terminal; orphaned: none; Done: none.
        let term = |r: &WorktreeRow| match &r.owner {
            RowOwner::Card { terminal, .. } => terminal.clone(),
            RowOwner::None => None,
        };
        assert_eq!(term(by_name(&live)).as_deref(), Some("t1"));
        assert_eq!(
            term(by_name(&orphan)),
            None,
            "an orphaned claim offers no terminal"
        );
        assert_eq!(term(by_name(&done)), None);
        // Attention: ahead is normal while In Progress, attention once Done;
        // a dirty stray is attention, an unprobed one is not.
        assert!(!by_name(&live).attention);
        assert!(by_name(&done).attention);
        assert!(!by_name("aaa111").attention);
        assert!(by_name("zzz999").attention);
        assert_eq!(by_name("aaa111").owner, RowOwner::None);

        let (text, attention) = worktree_strip(&rows).unwrap();
        assert_eq!(text, "5 worktrees · 1 dirty · 2 no card");
        assert!(attention);
        let (text, attention) = worktree_strip(&rows[..1]).unwrap();
        assert_eq!(text, "1 worktree");
        assert!(!attention);
        assert!(worktree_strip(&[]).is_none());
    }

    #[test]
    fn status_poll_runs_with_no_worktree_cards_and_keeps_strays_a_card_has_not_claimed() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_at(tmp.path());
        let t0 = std::time::Instant::now();
        store.mark_shown(t0);
        // No card has a worktree: the round still runs (it lists strays).
        let batch = store.take_status_poll(t0).unwrap();
        assert!(batch.is_empty());
        assert!(
            store
                .take_status_poll(t0 + STATUS_POLL_INTERVAL * 2)
                .is_none(),
            "in flight"
        );
        let stray = StrayWorktree {
            wt: sample_worktree(),
            status: None,
        };
        store.set_worktree_statuses(Default::default(), vec![stray.clone()]);
        assert_eq!(store.strays(), &[stray.clone()]);
        // A card that claimed that path since the snapshot un-strays it.
        let id = store.add("claims it", None).unwrap();
        store
            .claim_for_dispatch(
                &id,
                "t1",
                "claude",
                run_nonce(),
                TermState::Missing,
                Some(sample_worktree()),
            )
            .unwrap();
        store.set_worktree_statuses(Default::default(), vec![stray]);
        assert!(store.strays().is_empty());
    }

    #[test]
    fn live_worktree_rows_union_cards_and_listing_and_probe_each_once() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = store_at(tmp.path());
        let wt_for = |id: &str| Worktree {
            path: format!("H:/repo/.foreman/worktrees/{id}"),
            branch: format!("card/{id}"),
            base: "main".into(),
            in_place: false,
        };
        // A card whose tree git no longer lists (pruned) still gets a row.
        let pruned = store.add("pruned", None).unwrap();
        store
            .claim_for_dispatch(
                &pruned,
                "t1",
                "claude",
                run_nonce(),
                TermState::Missing,
                Some(wt_for(&pruned)),
            )
            .unwrap();
        let live = store.add("live", None).unwrap();
        store
            .claim_for_dispatch(
                &live,
                "t2",
                "claude",
                run_nonce(),
                TermState::Missing,
                Some(wt_for(&live)),
            )
            .unwrap();
        let listed = vec![wt_for(&live), wt_for("stray1")];
        let probed = std::cell::RefCell::new(Vec::new());
        let rows = live_worktree_rows(store.cards(), &Default::default(), listed, |wt| {
            probed.borrow_mut().push(wt.path.clone());
            if wt.path.ends_with("stray1") {
                None
            } else {
                Some(WorktreeStatus {
                    missing: wt.path.ends_with(&pruned),
                    ..Default::default()
                })
            }
        });
        let mut names: Vec<String> = rows.iter().map(WorktreeRow::name).collect();
        let stray_pos = names.iter().position(|n| n == "stray1").unwrap();
        assert_eq!(stray_pos, 2, "stray last");
        names.sort();
        let mut want = vec![pruned.clone(), live.clone(), "stray1".into()];
        want.sort();
        assert_eq!(names, want);
        let mut p = probed.borrow().clone();
        p.sort();
        let mut want_p = vec![
            wt_for(&pruned).path,
            wt_for(&live).path,
            wt_for("stray1").path,
        ];
        want_p.sort();
        assert_eq!(p, want_p, "each tree probed exactly once");
        let pruned_row = rows.iter().find(|r| r.name() == pruned).unwrap();
        assert!(pruned_row.status.unwrap().missing);
        let stray_row = &rows[2];
        assert_eq!(stray_row.owner, RowOwner::None);
        assert!(
            stray_row.status.is_none(),
            "an errored probe is absent, not zero"
        );

        // Lines: the wire shape omits absent card/status; humans get the
        // same `[wt …]` tail as `list`.
        let line = WorktreeLine::from_row(stray_row);
        let j = line.json_line();
        assert!(!j.contains("\"card\"") && !j.contains("\"status\""), "{j}");
        assert_eq!(line.human_line(), "stray1  no card  [wt card/stray1]");
        let back: WorktreeLine = serde_json::from_str(&j).unwrap();
        assert_eq!(back, line);
        let line = WorktreeLine::from_row(rows.iter().find(|r| r.name() == live).unwrap());
        assert_eq!(
            line.human_line(),
            format!("{live}  in_progress  live  [wt card/{live} +0 -0]")
        );
        let j = line.json_line();
        assert!(
            j.contains("\"card\":{\"id\":") && j.contains("\"state\":\"in_progress\""),
            "{j}"
        );
    }
}
