# Kanban Board Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A per-project kanban board (Backlog / In Progress / Blocked / Done) with file-per-card storage in `.foreman/tasks/`, `foreman kanban` CLI verbs over the control pipe, dispatch-from-card into a new Session, derived orphan detection, and a `foreman-kanban` embedded skill.

**Architecture:** New pure module for the card domain (store, transitions, orphan derivation, id/nonce generation, prompt template, wait verdicts) + new view module for the board window. One additive control-plane verb (`cmd: "kanban"` with an `action` discriminator) rides the existing pipe; the GUI is the single writer and validates every transition. The board window is a new window-content variant following the chat-viewer pattern (per-project singleton, shared store via `Rc`, intents drained after the apply pass).

**Tech Stack:** Rust (edition 2024), egui 0.34.3, serde/serde_json, chrono (timestamps), sha2 (id/nonce derivation). **Zero new dependencies.**

**Spec:** `docs/superpowers/specs/2026-08-28-kanban-board-design.md` (design; read it first) and `docs/superpowers/specs/2026-08-28-kanban-board-brainstorm.md` (decision history). Everything in both is settled with the user — do not re-litigate.

## Global Constraints

- **No new dependencies.** Ids and the run nonce derive from `sha2` over time + pid + counter (both crates already in `Cargo.toml`). Do not add `rand`, `uuid`, or anything else.
- **Wire compat v1 is byte-identical.** The new request is additive (old exe answers `unknown cmd: kanban`). Every new reply field carries `#[serde(default, skip_serializing_if = ...)]` plus a compat test modeled on `chat_request_to_is_wire_compatible_with_v1` in `src/control.rs`.
- **Build without touching a running exe.** If `$env:FOREMAN` is `1` you are INSIDE foreman: build with `cargo build --target-dir target/agent`, and NEVER `Stop-Process -Name foreman` (kills the user's installed instance). The Bash-tool PreToolUse hook kills repo-built instances for you; the PowerShell tool does not.
- **Tests are module-local** `#[cfg(test)] mod tests`; run per-module (`cargo test kanban::`), full suite as the final gate. Test names are behavior sentences in snake_case.
- **GUI claims need image evidence.** The build-screenshot skill is user-only — at each GUI checkpoint, ask Andy to run it; do not claim visual behavior otherwise.
- **Commit per task, staging files by name** (never `git add -A`). Subject `type(scope): imperative summary`; body says why + an evidence line; trailer `Co-Authored-By: Claude <model> <noreply@anthropic.com>`. Verify with `git log -1 --format=%B` after each commit.
- **Cite-guard** runs on every `.md` edit under `docs/` and `.claude/skills/`: no line numbers, no counts; cite file + symbol. Docs written by this plan must follow it.
- **Vocabulary:** board / card / claim (CONTEXT.md entries land with their seam commits, Tasks 1 and 4). Never "task" unqualified — "task manager" is the window-switcher panel.
- The `.foreman/tasks/` card files are written ONLY via atomic temp-file-then-rename (the `write_managed_file_if_changed` pattern in `src/skills_install.rs`), so a reader never sees a half-written card.

---

### Task 1: Pure card domain — new module `src/kanban.rs`

**Files:**
- Create: `src/kanban.rs`
- Modify: `src/main.rs` (module declaration), `CONTEXT.md` (card + claim entries)

**Interfaces:**
- Consumes: nothing project-specific (std, serde, chrono, sha2).
- Produces (later tasks call these exact names):
  - `pub const CARD_V: u32` (= 1), `pub const POLL_INTERVAL: std::time::Duration` (= 2 s)
  - `pub enum CardState { Backlog, InProgress, Blocked, Done }` (serde `rename_all = "snake_case"`, so the wire strings are `backlog` / `in_progress` / `blocked` / `done`)
  - `pub struct Claim { terminal: String, run: String, agent: Option<String>, at: String }` (all pub fields)
  - `pub struct Card { v, id, title, body: Option<String>, state, blocked_reason: Option<String>, claim: Option<Claim>, created, updated }` (all pub fields; `body`/`blocked_reason`/`claim` are `skip_serializing_if = "Option::is_none"`)
  - `pub enum TermState { Missing, Running, Exited }`
  - `pub fn run_nonce() -> &'static str`
  - `pub fn claim_is_dead(claim: Option<&Claim>, current_run: &str, term: TermState) -> bool`
  - `pub fn is_orphaned(card: &Card, current_run: &str, states: &std::collections::HashMap<String, TermState>) -> bool`
  - `pub struct CardStore` with methods `set_dir`, `reload`, `maybe_reload`, `cards`, `get`, `add`, `start`, `claim_for_dispatch`, `done`, `block`, `release`, `rm`, `set_orphans`, `orphans`, `mark_shown`, `shown_recently` (exact signatures in Step 4)
  - `pub fn dispatch_prompt(card: &Card) -> String`
  - `pub struct CardLine { pub card: Card, pub orphaned: bool }` (serde `flatten` on `card`) with `pub fn human_line(&self) -> String` and `pub fn json_line(&self) -> String`
  - `pub enum WaitTarget { Id(String), Any }`
  - `pub fn wait_verdict(target: &WaitTarget, watched: &mut std::collections::HashSet<String>, cards: &[CardLine]) -> Option<i32>`

- [ ] **Step 1: Create the module skeleton and register it.**

Create `src/kanban.rs` with a module doc explaining why it exists (card domain for the per-project board: file-per-card storage under `.foreman/tasks/`, single-writer transitions, derived orphan state — GUI-free and fully unit-testable; cite the spec path `docs/superpowers/specs/2026-08-28-kanban-board-design.md`). Add `mod kanban;` to `src/main.rs` beside the other module declarations.

- [ ] **Step 2: Card schema types + id/nonce generation (write the failing tests first).**

Tests to write first, in `#[cfg(test)] mod tests`:

```rust
#[test]
fn card_json_matches_spec_shape_and_omits_empty_options() {
    let c = Card::new("a3f8k2".into(), "Fix resize flicker".into(), None, "2026-08-28T13:55:00Z".into());
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
        assert!(id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()), "{id}");
        assert!(seen.insert(id), "collision not regenerated");
    }
}

#[test]
fn run_nonce_is_stable_within_the_process() {
    assert_eq!(run_nonce(), run_nonce());
    assert!(!run_nonce().is_empty());
}
```

Run `cargo test kanban::` — expect FAIL (types missing). Then implement:

- `Card` / `Claim` / `CardState` exactly as the Interfaces block above; `Card::new(id, title, body, now) -> Card` sets `v: CARD_V`, `state: Backlog`, `created == updated == now`.
- Timestamps: `chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)` behind a small `fn now_stamp() -> String`.
- `run_nonce()`: `std::sync::OnceLock<String>`; first call hashes (`sha2::Sha256`) the epoch nanos + `std::process::id()`, hex-encodes, truncates to 32 chars. It is an app-instance nonce — uniqueness per launch is the contract, not cryptographic strength.
- `fn gen_id(existing: &std::collections::HashSet<String>) -> String`: hash `run_nonce()` + a `static AtomicU64` counter + epoch nanos; map the digest into 6 base36 chars (`0-9a-z`); loop while the result is in `existing` (single writer, so regenerate-on-hit is the whole collision story per spec).

Run `cargo test kanban::` — expect PASS.

- [ ] **Step 3: Transition rules + orphan derivation (tests first — these are the prime state-machine tables).**

Transition contract (from the spec — enforced identically for CLI and board writes):

| From | Verb | Result |
|---|---|---|
| Backlog | claim (start/dispatch) | InProgress + claim recorded |
| Blocked | claim (start/dispatch) | InProgress + claim recorded, `blocked_reason` cleared |
| InProgress + **dead** claim | claim | InProgress + claim replaced (seize) |
| InProgress + **live** claim | claim | **Error** (the two-agents-one-card guard) |
| InProgress | done | Done, claim cleared |
| InProgress | block(reason) | Blocked, claim cleared, reason recorded |
| InProgress or Blocked | release | Backlog, claim cleared |
| any | rm | file deleted |
| everything else | anything else | Error |

Tests first (transition-table style — every arm a named case or one table test, per the living example in `src/ready.rs` gate tests):

```rust
#[test]
fn claim_is_dead_covers_the_full_derivation_table() {
    let live = Claim { terminal: "t4".into(), run: "R".into(), agent: None, at: String::new() };
    // (claim, current_run, term_state) -> dead?
    let cases = [
        (None, "R", TermState::Running, true),                    // no claim
        (Some(&live), "OTHER", TermState::Running, true),         // stale run nonce
        (Some(&live), "R", TermState::Missing, true),             // terminal gone
        (Some(&live), "R", TermState::Exited, true),              // terminal exited
        (Some(&live), "R", TermState::Running, false),            // alive
    ];
    for (claim, run, term, want) in cases {
        assert_eq!(claim_is_dead(claim, run, term), want, "{claim:?} {run} {term:?}");
    }
}

#[test]
fn is_orphaned_only_fires_on_in_progress_cards() {
    // Done/Backlog/Blocked cards are never orphaned even with a dead claim;
    // an InProgress card with a dead claim is.
    // Build one card per state with a claim whose run nonce is stale; assert.
}

#[test]
fn start_rejects_a_live_claim_and_seizes_a_dead_one() { /* via CardStore in Step 4 */ }
```

Implement `claim_is_dead` and `is_orphaned` (`is_orphaned` = `state == InProgress` && `claim_is_dead(...)` where the terminal's `TermState` comes from the `states` map, `Missing` when absent). Run tests — PASS.

- [ ] **Step 4: CardStore — load/save/staleness + the verb methods (tests first, `tempfile` dev-dep is already in the tree).**

```rust
pub struct CardStore {
    dir: Option<std::path::PathBuf>,            // <project cwd>/.foreman/tasks
    cards: Vec<Card>,                           // sorted by (created, id)
    fingerprint: Vec<(String, std::time::SystemTime, u64)>, // (file name, mtime, len), sorted
    last_poll: Option<std::time::Instant>,
    last_shown: Option<std::time::Instant>,     // stamped by the board view each rendered frame
    orphans: std::collections::HashSet<String>, // refreshed each frame by the wm tick (Task 3)
}
```

Visibility stamp (consumed in Tasks 3–4): `mark_shown(&mut self, now: std::time::Instant)` records the stamp; `shown_recently(&self, now: std::time::Instant) -> bool` is true within 1 s of the last stamp — it gates the staleness poll to a board that is actually being rendered.

Method contracts (all `&mut self` unless stated; every mutation re-reads the target file first when it exists — files are authoritative over memory — writes atomically, then updates `cards` + `fingerprint`):

- `set_dir(&mut self, project_cwd: Option<&std::path::Path>)` — sets `dir` to `cwd/.foreman/tasks` (or `None`); on change, `reload`. Idempotent, called every tick.
- `reload(&mut self)` — read every `*.json` in `dir` into `cards` (skip unparseable files with an `eprintln!`, never panic), refresh `fingerprint`. Missing dir = empty store (the dir is only created on first `add`).
- `maybe_reload(&mut self, now: std::time::Instant)` — no-op unless `POLL_INTERVAL` has elapsed since `last_poll`; then compare a fresh fingerprint against the stored one and `reload` only on mismatch. This is the branch-switch/pull staleness poll from the spec's Reconciliation section.
- `cards(&self) -> &[Card]`, `get(&self, id: &str) -> Option<&Card>`, `orphans(&self) -> &std::collections::HashSet<String>`, `set_orphans(&mut self, o: std::collections::HashSet<String>)`.
- `add(&mut self, title: &str, body: Option<&str>) -> Result<String, String>` — reject empty/whitespace title; `gen_id` against existing ids; create dir if missing; write; return the id.
- `start(&mut self, id: &str, terminal: &str, current_run: &str, term: TermState) -> Result<(), String>` — the claim verb for self-service pickup: allowed from Backlog, Blocked, or InProgress-with-dead-claim (checked via `claim_is_dead` with `term` = the state of the *existing* claim's terminal); records `Claim { terminal, run: current_run, agent: None, at: now }`, state → InProgress, clears `blocked_reason`.
- `claim_for_dispatch(&mut self, id: &str, terminal: &str, agent: &str, current_run: &str, term: TermState) -> Result<(), String>` — same transition rules as `start` but records `agent: Some(agent)`. Called by the board's dispatch drain after a successful spawn (dispatch claims atomically; a card-spawned agent never runs `start`).
- `done(&mut self, id: &str) -> Result<(), String>` — InProgress → Done only; clears claim. Missing card file = error, never create (close-out never resurrects a card).
- `block(&mut self, id: &str, reason: &str) -> Result<(), String>` — InProgress → Blocked only; reject empty reason; clears claim, records reason.
- `release(&mut self, id: &str) -> Result<(), String>` — InProgress or Blocked → Backlog; clears claim + reason. (Board-only recovery action; deliberately NOT a wire verb — the spec's verb table is closed.)
- `rm(&mut self, id: &str) -> Result<(), String>` — delete the file, any state; error if missing.
- No claimant check on `done`/`block`/`rm` — same trust model as chat (guardrail, not a security boundary).

Tests first (use `tempfile::tempdir()`):

```rust
#[test]
fn add_then_reload_roundtrips_a_card_file() { /* add; new store on same dir; reload; assert equal */ }

#[test]
fn full_transition_table_is_enforced() {
    // Drive one store through: add -> start ok -> start again (live claim) ERR ->
    // done ok -> done again ERR (Done is terminal) -> add -> start -> block("r") ok ->
    // done from Blocked ERR -> start from Blocked ok (re-claim) -> release ok (-> Backlog) ->
    // done from Backlog ERR -> rm ok. Assert claim/reason fields clear correctly.
}

#[test]
fn block_demands_a_nonempty_reason() { /* ERR on "" and "  " */ }

#[test]
fn closeout_on_a_missing_card_errors_and_creates_nothing() {
    // done/block/rm on an id with no file: Err, and the dir gains no file.
}

#[test]
fn start_seizes_an_orphaned_claim_but_rejects_a_live_one() {
    // claim with run_nonce()+Running term: second start ERR.
    // claim with stale run: start succeeds and replaces the claim.
}

#[test]
fn maybe_reload_picks_up_external_file_changes_after_the_interval() {
    // write a card file behind the store's back; maybe_reload with a now past
    // POLL_INTERVAL sees it; a deleted file drops off. (Construct `last_poll`
    // directly or pass timestamps; never sleep-and-hope.)
}

#[test]
fn updated_stamp_moves_on_every_transition() { /* created stays, updated changes */ }
```

Implement, run `cargo test kanban::` — PASS.

- [ ] **Step 5: Prompt template + list lines + wait verdicts (tests first).**

`dispatch_prompt` renders the spec's template **verbatim** (fixed, card fields interpolated, nothing else):

```rust
pub fn dispatch_prompt(card: &Card) -> String {
    format!(
        "You are a worker Session dispatched from card {id} on this project's board.\n\
         \n\
         # Task: {title}\n\
         \n\
         {body}\n\
         \n\
         # Close-out (required)\n\
         When the work is complete, run:    foreman kanban done {id}\n\
         If you are stuck and need a human: foreman kanban block {id} --reason \"<one line>\"\n\
         Do not end the session without running one of these.",
        id = card.id,
        title = card.title,
        body = card.body.as_deref().unwrap_or(""),
    )
}
```

`CardLine { #[serde(flatten)] pub card: Card, pub orphaned: bool }`:
- `json_line()` — one JSON object per line (`serde_json::to_string`), the agent-facing `list --json` format; the derived `orphaned` flag exists only here, never in the card files.
- `human_line()` — one aligned human line: id, state, title, then a context tail: `orphaned` marker when orphaned, `[claimed tN agent]` while claimed, `(reason)` when blocked. Example: `a3f8k2  in_progress  Fix resize flicker  [t4 claude] ORPHANED`.

`wait_verdict` (pure — the CLI loop in Task 2 drives it):
- `WaitTarget::Id(id)`: card Done → `Some(0)`; Blocked or orphaned → `Some(1)`; **missing** → `Some(1)` (something needs a human — the card was removed under the waiter); Backlog or live InProgress → `None` (keep waiting).
- `WaitTarget::Any`: first, insert every live InProgress card id into `watched`; then for each previously watched id: now Done → `Some(0)`; Blocked / orphaned / missing → `Some(1)`; else `None`. (Cards never watched — e.g. sitting in Backlog — never trigger.)

Tests: a golden-string test for `dispatch_prompt` (body present and `None`), `json_line` round-trips back through serde and carries `"orphaned":`, `human_line` shows the blocked reason and orphan marker, and a `wait_verdict` table test per target covering every row above (including “--any watches a card only after seeing it in progress”).

Run `cargo test kanban::` — PASS.

- [ ] **Step 6: CONTEXT.md entries (card, claim).**

Add to CONTEXT.md, matching the file's entry shape (bold term, 1–3 line meaning, `_Avoid_:` line):
- **Card** — one unit of work-in-flight on a project's board; a file in `.foreman/tasks/` owned by the app. _Avoid_: task (taken), ticket, issue (GitHub's word).
- **Claim** — the card↔Session link recorded at dispatch or `start`: terminal id, app run nonce, agent, timestamp. Dead claims are derived (orphan), never stored. _Avoid_: assignment, lock.

(**Board** lands with Task 4's seam commit.)

- [ ] **Step 7: Gate + commit.**

Run: `cargo test kanban::` then `cargo check` (clean modulo the known warning baseline). Stage `src/kanban.rs`, `src/main.rs`, `CONTEXT.md` by name. Commit:

```
feat(kanban): pure card domain — store, transitions, derived orphans

File-per-card store for .foreman/tasks with the spec's transition table,
derived orphan check (run nonce + terminal state), base36 id generation
without new deps, dispatch prompt template, and wait verdicts.
Evidence: cargo test kanban:: green.

Co-Authored-By: Claude <model> <noreply@anthropic.com>
```

---

### Task 2: Wire + CLI — `foreman kanban` over the pipe

**Files:**
- Modify: `src/control.rs`

**Interfaces:**
- Consumes: `crate::kanban::{CardLine, WaitTarget, wait_verdict}` (Task 1).
- Produces: `KanbanRequest` (all pub fields per the spec's wire shape), `CtrlMsg::Kanban(KanbanRequest, mpsc::Sender<OpenReply>, std::time::Instant)`, `OpenReply` gains `pub id: Option<String>`, `pub fn parse_kanban_args(args, default_project, self_terminal) -> Result<KanbanAction, String>` where `KanbanAction` is either a wire request or a client-side wait spec. Task 3 consumes `CtrlMsg::Kanban`.

- [ ] **Step 1: Wire structs (tests first).**

Tests first, alongside the existing wire tests in `src/control.rs`:

```rust
#[test]
fn open_reply_id_is_wire_compatible_with_v1() {
    // unset id serializes away (byte-identical to v1)…
    let ok = OpenReply { ok: true, ..Default::default() };
    assert!(!serde_json::to_string(&ok).unwrap().contains("\"id\""));
    // …and a v1 reply without the key still parses.
    let r: OpenReply = serde_json::from_str(r#"{"ok":true}"#).unwrap();
    assert_eq!(r.id, None);
}

#[test]
fn kanban_request_wire_roundtrips_and_omits_unset_fields() {
    let req = KanbanRequest { cmd: "kanban".into(), action: "list".into(), ..Default::default() };
    let j = serde_json::to_string(&req).unwrap();
    assert!(!j.contains("\"id\"") && !j.contains("\"title\"") && !j.contains("\"json\""));
    let back: KanbanRequest = serde_json::from_str(&j).unwrap();
    assert_eq!(back, req);
}
```

Implement, following the spec's wire shape exactly:

```rust
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct KanbanRequest {
    pub cmd: String,                 // always "kanban"
    pub action: String,              // "add" | "list" | "start" | "done" | "block" | "rm"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,     // None = caller's FOREMAN_PROJECT_ID, else focused
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,        // caller's FOREMAN_TERMINAL_ID; required for start
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,       // list filter
    #[serde(default, skip_serializing_if = "is_false")]
    pub json: bool,                  // list output mode
}
```

Add to `OpenReply` (beside the `seq` field, same pattern):

```rust
    /// The id `kanban add` created. Skipped on the wire when None so v1
    /// replies stay byte-identical (same pattern as `seq`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
```

Add `Kanban(KanbanRequest, mpsc::Sender<OpenReply>, std::time::Instant)` to `CtrlMsg` and a `"kanban"` arm to the `serve` match (copy the `"view"` arm's shape). Run the two tests — PASS. (`handle_ctrl` won't compile until Task 3 adds its arm — add a temporary arm that replies `OpenReply::err("kanban: not wired yet")` so this task stands alone, and note it for Task 3 to replace.)

- [ ] **Step 2: Arg parsing (tests first).**

`parse_kanban_args` returns an enum so `wait` never becomes a wire request:

```rust
pub enum KanbanAction {
    Request(KanbanRequest),
    Wait { project: Option<String>, target: crate::kanban::WaitTarget, timeout: Option<u64> },
}
```

Grammar (`foreman kanban <action> ...`; every action takes `--project P` overriding the env default):
- `add <title words...> [--body B]` — positional words join (space-separated) into the title; error on empty title.
- `list [--state backlog|in_progress|blocked|done] [--json]` — invalid state errors client-side.
- `start <id>` — requires `self_terminal` (FOREMAN_TERMINAL_ID) → `from`; error outside a foreman terminal.
- `done <id>` / `rm <id>` — one positional id.
- `block <id> --reason R` — `--reason` mandatory and non-empty (a Blocked column without reasons costs the human an investigation per card).
- `wait <id>` or `wait --any`, `[--timeout SECS]` — exactly one of id/`--any`.
- Anything else: `unknown kanban action: <x>`.

Tests first: one per action happy path, plus `block` without `--reason` errors, `start` without env errors, `wait` with both id and `--any` errors, unknown action/flag errors, title joining. Implement (follow the style of `parse_send_args`). Run — PASS.

- [ ] **Step 3: Client entry, help, and the wait loop.**

- Add `Some("kanban") => kanban_main(&args[1..])` to `client_main`, a `HELP_KANBAN` const (model on `HELP_CHAT`; document every action, the exit codes, and that `list --json` emits one JSON card per line with the derived `orphaned` flag), a kanban line in the top-level `HELP` and the usage `eprintln!` block.
- `kanban_main`: `--help` first (must work outside a foreman terminal), then parse. `KanbanAction::Request` → `report("foreman kanban", request(PIPE, &req))` (the `id` field rides the JSON ok-reply that `report` already prints; `list` output rides `history` and prints line-per-line).
- `KanbanAction::Wait` → `kanban_wait(project, target, timeout) -> i32`:

```rust
fn kanban_wait(project: Option<String>, target: crate::kanban::WaitTarget, timeout: Option<u64>) -> i32 {
    let deadline = timeout.map(|s| std::time::Instant::now() + std::time::Duration::from_secs(s));
    let mut watched = std::collections::HashSet::new();
    loop {
        let req = KanbanRequest {
            cmd: "kanban".into(), action: "list".into(),
            project: project.clone(), json: true, ..Default::default()
        };
        match request(PIPE, &req) {
            // Spec exit-code contract: 2 = timeout or foreman unreachable
            // (deliberately different from other verbs' unreachable=1).
            Err(e) => { eprintln!("foreman kanban wait: cannot reach foreman ({e})"); return 2; }
            Ok(r) if !r.ok => { eprintln!("foreman kanban wait: {}", r.error.unwrap_or_default()); return 1; }
            Ok(r) => {
                let cards: Vec<crate::kanban::CardLine> = r.history.unwrap_or_default().iter()
                    .filter_map(|l| serde_json::from_str(l).ok()).collect();
                if let Some(code) = crate::kanban::wait_verdict(&target, &mut watched, &cards) {
                    return code;
                }
            }
        }
        if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
            eprintln!("foreman kanban wait: timeout");
            return 2;
        }
        std::thread::sleep(std::time::Duration::from_secs(2)); // client-side poll: the pipe
        // server is serial; a held connection would wedge dispatch for every agent (spec).
    }
}
```

- [ ] **Step 4: Pipe round-trip test.**

Copy the `close_pipe_roundtrip` shape (unique pipe name `foreman-test-kanban-{pid}`, retry-while-binding, fake GUI thread answers `CtrlMsg::Kanban` for an `add` with `ok + id`), assert the client sees the id. Add `kanban help` to `help_prints_and_exits_zero_everywhere`.

- [ ] **Step 5: Gate + commit.**

`cargo test control::` green, `cargo check` clean. Stage `src/control.rs`. Commit `feat(control): kanban wire verb + CLI with client-side wait` (body: additive cmd, OpenReply id field wire-compat evidence line).

---

### Task 3: Server dispatch + store wiring in the window manager

**Files:**
- Modify: `src/wm.rs`, `src/main.rs`

**Interfaces:**
- Consumes: `CtrlMsg::Kanban` (Task 2), `CardStore` + `is_orphaned` + `TermState` + `CardLine` (Task 1), existing `resolve_project` / `project_child_mut` / `term_tag` / `term_id` in `src/wm.rs`.
- Produces: a `kanban` store field on the window manager (`Rc<RefCell<crate::kanban::CardStore>>`), `pub fn kanban_tick(&mut self)`, `fn term_states(&self) -> std::collections::HashMap<String, crate::kanban::TermState>`, `fn kanban_dispatch(&mut self, req: &crate::control::KanbanRequest) -> Result<crate::control::OpenReply, String>`. Task 4 consumes the store field and `term_states`.

- [ ] **Step 1: Store field + per-frame tick.**

- Add to the `WindowManager` struct (beside the `chat` field, same doc style): `kanban: Rc<RefCell<crate::kanban::CardStore>>`, initialized in `WindowManager::new` with `CardStore::default()`. Derive or hand-write `Default` for the store. A desktop manager's store stays dir-less and inert — same "harmless at desktop level" posture as the chat room.
- `fn term_states(&self)` — walk this manager's windows/tabs; for each `Content::Terminal`, insert `term_tag(id)` → `Running` / `Exited` (from the session's `exited()`); absent = `Missing` by lookup convention.
- The store gains a visibility stamp: a `last_shown: Option<std::time::Instant>` field with `mark_shown(&mut self, now)` / `shown_recently(&self, now) -> bool` (true within ~1 s). The board view stamps it every rendered frame (Task 4), so a minimized or background board stops the poll — the spec's "nothing while hidden" — with zero window-state plumbing. Until Task 4 exists nothing stamps it, so the poll is naturally inert.
- `pub fn kanban_tick(&mut self)` — mirror the recursion shape of `chat_tick`: recurse into every `Content::Project` child first; then, on managers with a `cwd`: `set_dir(cwd)`, and only when `shown_recently` reports a live board view call `maybe_reload(Instant::now())`. Finally recompute the orphan set: `is_orphaned(card, run_nonce(), &self.term_states())` per card → `set_orphans`. Call `self.desktop.kanban_tick()` in `src/main.rs` at BOTH call sites where `chat_tick` runs (the visible-frame path and the hidden/occluded logic path), right after `chat_tick`.

- [ ] **Step 2: The dispatch arm (tests after — they need the store wired).**

Replace Task 2's placeholder `CtrlMsg::Kanban` arm in `handle_ctrl`:

```rust
CtrlMsg::Kanban(req, reply, sent) => {
    if sent.elapsed() >= REPLY_TIMEOUT {
        return; // stale: the client was already told "foreman did not respond"
    }
    let _ = reply.send(match self.kanban_dispatch(&req) {
        Ok(r) => r,
        Err(e) => OpenReply::err(e),
    });
}
```

`kanban_dispatch`: resolve the project (`resolve_project(req.project.as_deref())`, same None-means-focused rule as `open`), get the child manager, `set_dir` from its cwd (a project with no cwd errors: "project has no working directory"), then match `req.action.as_str()`:

- `"add"` — title required; `store.add(title, body)`; reply `OpenReply { ok: true, id: Some(new_id), .. }`.
- `"list"` — `reload()` first (files are authoritative; a CLI list must never serve a stale in-memory copy), compute orphans via `is_orphaned` + `term_states`, optional state filter (parse the filter against the serde names; unknown filter errors), build `CardLine` per card, reply `history` = `json_line` or `human_line` per `req.json`.
- `"start"` — `from` required (error: "start requires FOREMAN_TERMINAL_ID (run inside a foreman terminal)"); the claimant must resolve to a **running** terminal in this project (via `term_states`) — a claim by an exited/unknown terminal would be born orphaned; then `store.start(id, from, run_nonce(), state_of_existing_claim_terminal)`.
- `"done"` / `"block"` / `"rm"` — plain store calls (`block` re-checks reason non-empty server-side; the app validates every transition and rejects garbage with a clean error, whatever the client said).
- other → `Err(format!("unknown kanban action: {a}"))`.

All verbs: reply `ok: true` with no extra fields unless stated. After any successful write, `ctx.request_repaint()` so an open board repaints on the app's own write (add `ctx` to the signature or call it from the `handle_ctrl` arm on `Ok`).

- [ ] **Step 3: Dispatch-level tests (PTY discipline applies).**

In the `src/wm.rs` test module, using the existing `pause_argv()` fixture + `egui::Context::default()`, a tempdir as the project cwd:

```rust
#[test]
fn kanban_add_list_roundtrip_reports_backlog() { /* build desktop + project with cwd=tempdir;
    kanban_dispatch add; assert id in reply; list --json; parse CardLine; state backlog, not orphaned */ }

#[test]
fn kanban_start_records_claim_and_second_start_is_rejected() { /* spawn a pause terminal, pump
    until ready (canonical fresh-Session pattern), start with its tag: ok; start again from
    another terminal: Err mentioning the live claim */ }

#[test]
fn kanban_card_orphans_when_its_terminal_exits_and_start_seizes_it() { /* start; inject a byte so
    `cmd /c pause` exits; pump until exited; kanban_tick; list shows orphaned:true; start from a
    second live terminal succeeds (seize) */ }

#[test]
fn kanban_closeout_verbs_enforce_the_table_over_the_wire_path() { /* done on backlog Err;
    block without reason Err; done after start ok; rm ok; done on missing id Err */ }
```

Deadline-bounded pump loops, never sleep-and-hope (the chat-delivery test pattern). Run `cargo test wm::kanban` — PASS.

- [ ] **Step 4: Gate + commit.**

`cargo test wm::` and `cargo test control::` green; `cargo check` clean. Stage `src/wm.rs`, `src/main.rs`. Commit `feat(wm): kanban server dispatch, store wiring, derived-orphan tick`.

---

### Task 4: The board window

**Files:**
- Create: `src/board.rs`
- Modify: `src/wm.rs`, `src/main.rs` (module decl), `src/workspace.rs`, `src/keymap.rs`, `CONTEXT.md` (board entry)

**Interfaces:**
- Consumes: `CardStore` (shared `Rc` from Task 3), `dispatch_prompt`, `surface_target`, `add_terminal_cmd`, `claim_for_dispatch`.
- Produces: `BoardView` (view type in `src/board.rs`), a new `Content` variant holding it, a `ContentSnap` variant persisting it, `Command::OpenBoard` (default chord: leader then plain `K`), `fn open_board_window(&mut self)`, `fn drain_board_acts(&mut self, ctx: &egui::Context)`.

- [ ] **Step 1: BoardView skeleton + intents.**

`src/board.rs` — module doc: the per-project board surface; read seam is the shared store snapshot, write seam is intents drained by the manager after the apply pass (the chat viewer's click pattern, the panel's act pattern — see `docs/task-manager-panel.md`).

```rust
pub const AGENTS: &[&str] = &["claude", "codex", "grok"]; // the set foreman already
// detects for tab icons and skill installs; no persisted default (spec).

/// One user intent recorded during the draw; drained by the window manager
/// after apply_acts (content cannot mutate the manager mid-loop).
pub enum BoardAct {
    QuickAdd(String),                 // title-only add into Backlog
    Dispatch { id: String, agent: String },
    Done(String),
    Release(String),                  // orphaned card -> back to Backlog
    Rm(String),
    JumpTo(String),                   // claimed terminal tag ("t4") — surface it
}

pub struct BoardView {
    store: std::rc::Rc<std::cell::RefCell<crate::kanban::CardStore>>,
    quick_add: String,                // Backlog header input buffer
    picker: Option<String>,           // card id whose agent picker is open
    scroll: [f32; 4],                 // per-column scroll offsets
    pub acts: Vec<BoardAct>,          // drained by the manager each frame
}

impl BoardView {
    pub fn new(store: std::rc::Rc<std::cell::RefCell<crate::kanban::CardStore>>) -> Self { /* … */ }
    pub fn show(&mut self, ui: &mut egui::Ui, rect: egui::Rect, active: bool,
                resp: &egui::Response, base: egui::Id) { /* Step 2 */ }
}
```

- [ ] **Step 2: Rendering.**

Four fixed columns (Backlog / In Progress / Blocked / Done), equal widths across `rect`, column header + count, per-column vertical scroll. Cards sorted by `created`. Follow the repo's existing flat theme (`crate::theme`) and the panel's row-paint idiom for hover affordances; derive every egui Id from `base` (nested-project collision rule).

Per card: title (elided, `on_hover_text` full title when elided — the panel precedent), dim id, and a status line:
- claimed: the claim's terminal id + agent — clickable → `BoardAct::JumpTo(terminal)`;
- orphaned (id in `store.orphans()`): an unmissable `ORPHANED` marker (theme warning color);
- blocked: the reason text.

Hover actions per spec — orphaned cards: **re-dispatch** (opens the picker) and **send back to Backlog** (`Release`); Backlog + Blocked cards: **dispatch** (picker); InProgress (live): **Done**; every card: **delete** (`Rm`). **No block button** (v1 has no card editor to type a reason into — CLI/agent's move). No drag-and-drop (fence).

Dispatch picker: an inline three-button row (`AGENTS`) shown on the picked card while `picker == Some(id)`; clicking an agent pushes `BoardAct::Dispatch` and closes the picker; clicking elsewhere closes it.

Quick-add: a single-line title input at the top of the Backlog column; Enter with non-empty text pushes `BoardAct::QuickAdd` and clears the buffer. (The settled "at most a title-only quick-add".)

Read path: `self.store.borrow()` once per frame into locals; drop the borrow before recording intents.

- [ ] **Step 3: The new Content variant, everywhere the compiler points.**

Add a `Board(crate::board::BoardView)` variant to the window-content enum in `src/wm.rs`. `cargo check` then enumerates every exhaustive match; the intended behavior per site:
- content `show` → `view.show(ui, rect, active, resp, base.with((win_id, "board")))`, returns false;
- `keepalive` → no-op (no PTY; the store is shared state);
- `icon_kind` → `None`; bell → false;
- `chat_tick` member scan → not a member (join the existing skip arm);
- `panel_model` row kind → reuse the chat viewer's row kind (a board row surfaces like any auxiliary tab; renaming the kind is out of scope);
- `status_dispatch` / terminal resolution / close paths → same treatment as the chat arm (skip);
- workspace capture → new unit variant `Board` in the snapshot enum in `src/workspace.rs` (beside `Chat`); restore arm mirrors the chat restore: rebuild the view with `Rc::clone` of the manager's store. Old workspace files simply lack the variant — no migration.
- `BoardView::show` calls the store's `mark_shown(Instant::now())` first thing each rendered frame — this is what arms Task 3's staleness poll only while the board is actually visible (spec: nothing while hidden; a minimized/background board goes quiet automatically).

- [ ] **Step 4: Singleton open + keybinding.**

- `fn open_board_window(&mut self)` — copy `open_chat_window` verbatim shape: if a board tab exists, `surface_target` it; else `push_win` a new window (`Tab::fixed("board", …)`, ~520×380 default slot) with `BoardView::new(Rc::clone(&self.kanban))`.
- `src/keymap.rs`: add `OpenBoard` to the command enum, `Command::ALL` (Terminals group, after `OpenChat`), `group()`, `label()` ("Open project board"), and the default table: `t.insert(plain(K::K), OpenBoard);` — plain `K` is unbound today (the vim-focus h/j/k/l defaults were deliberately dropped; the merge contract gives old keybindings.json files the new default automatically). Mirror the dispatch arm beside `Command::OpenChat` in `src/wm.rs`: `Command::OpenBoard => child.open_board_window()`.

- [ ] **Step 5: The board drain.**

`fn drain_board_acts(&mut self, ctx: &egui::Context)` on the manager, called in the desktop frame right beside `drain_chat_clicks` (and recursing into project children the same way that family of drains reaches nested managers — follow how `drain_chat_clicks` is reached for nested projects and do the same). Collect every view's `acts`, then apply per act **in this manager** (the project level — the store and terminals live here):

- `QuickAdd(title)` → `self.kanban.borrow_mut().add(&title, None)`; error → `eprintln!` (v1: no toast surface).
- `Done(id)` / `Release(id)` / `Rm(id)` → matching store calls; errors logged the same way (the validated-transition table protects the files).
- `JumpTo(tag)` → resolve the terminal by tag and `surface_target` it — copy the resolution loop from `drain_chat_clicks`.
- `Dispatch { id, agent }` →
  1. read the card (missing → log, done);
  2. `let prompt = crate::kanban::dispatch_prompt(&card);`
  3. `self.add_terminal_cmd(&[agent.clone(), prompt], None, Some(&card.title), ctx)` — cwd defaults to the project cwd inside `add_terminal_cmd`; the window title is the card title so the tab reads as the work;
  4. on `Ok(tid)`: `self.kanban.borrow_mut().claim_for_dispatch(&id, &term_tag(tid), &agent, crate::kanban::run_nonce(), /* existing-claim terminal state via term_states */)` — claim + move to InProgress in the same action (dispatch claims atomically); if the claim itself errors (e.g. the card was closed out mid-frame), close the just-spawned terminal (the open-undo precedent) and log;
  5. on spawn `Err` → log; card untouched.

Store writes repaint naturally (the frame is already running); no watcher needed for the common case.

- [ ] **Step 6: Tests.**

- `open_board_window_is_a_per_project_singleton` — call twice; one board tab, second call surfaces it (copy the chat-window singleton test if one exists, else the `ensure_panel_is_idempotent_and_tiled_right` shape).
- `board_dispatch_act_spawns_claims_and_titles_from_the_card` — tempdir project, add a card, push a `Dispatch` act with agent = the `pause_argv()` program (the act carries an argv-capable string; for the test, allow the act's agent to be any command — assert via the card's claim agent field and the spawned tab title == card title; PTY discipline: pump loops with deadlines).
- `board_release_act_returns_an_orphaned_card_to_backlog` — claim with a stale run nonce, tick, release act, assert Backlog + no claim.
- Workspace round-trip: extend the existing capture/restore tests with a board tab (unit variant restores against the project store).
- Keymap: extend the defaults test to assert plain `K` resolves to the new command.

Run `cargo test wm:: board:: keymap:: workspace::` — PASS.

- [ ] **Step 7: CONTEXT.md board entry + GUI evidence.**

- CONTEXT.md: **Board** — the per-project kanban surface (a window content variant); shows cards in four fixed columns. _Avoid_: kanban window, task board, panel (taken).
- Build (`cargo build`, or `--target-dir target/agent` when inside foreman) and **ask Andy to run build-screenshot**: board opens via leader `K`, four columns render, quick-add creates a card, a dispatched card shows its claim, killing the worker pane shows `ORPHANED`. Do not claim visual behavior without the screenshot readback.

- [ ] **Step 8: Gate + commit.**

Full `cargo test` green. Stage `src/board.rs`, `src/wm.rs`, `src/main.rs`, `src/workspace.rs`, `src/keymap.rs`, `CONTEXT.md`. Commit `feat(board): per-project kanban board window with dispatch-from-card`.

---

### Task 5: The foreman-kanban embedded skill

**Files:**
- Create: `.claude/skills/foreman-kanban/SKILL.md`, `.codex/skills/foreman-kanban/SKILL.md`, `.codex/skills/foreman-kanban/agents/openai.yaml`
- Modify: `src/skills_install.rs`

- [ ] **Step 1: Write the Claude skill.**

Frontmatter `name: foreman-kanban` (directory name MUST equal it); description gated on FOREMAN=1: "Use when running inside foreman (the FOREMAN env var is 1) and coordinating work through the project's kanban board — picking up a card, creating cards, closing out with done/block, or waiting on workers." Body opens with the stop-sign line (exact register of the dispatch skill): "**This skill is complete. Do NOT read foreman source or docs to learn kanban mechanics — every fact you need is below.**" Content, kept tight:
- Address the CLI as `& $env:FOREMAN_EXE kanban …` (PowerShell) / `"$FOREMAN_EXE" kanban …` (bash) — same convention as dispatch/chat.
- The verb table from the spec (add/list/start/done/block/rm/wait) with one-line examples; `--help` is ground truth for flags.
- Close-out discipline: a card you claimed ends with `done` or `block --reason` — never end a session without one; card-spawned agents already have the commands in their prompt.
- `start` on a claimed card is rejected — that is the guard, not an error to retry.
- The routing rule verbatim: changes a card's column → kanban verb; needs a reply from someone → chat. Durable content → GitHub Issues or docs; the card body is a pointer.
- Body convention: a few lines of task statement plus paths/issue numbers.

- [ ] **Step 2: Codex twin + yaml.**

Semantically synced copy adapted for Codex (env addressing identical; any agent-specific example uses `codex`), plus `agents/openai.yaml` matching the existing skills' yaml shape (`interface` block with display name/description — copy a sibling's structure).

- [ ] **Step 3: Embed + install.**

In `src/skills_install.rs`: three new `include_str!` consts (Claude SKILL.md, Codex SKILL.md, Codex openai.yaml), a bundle entry in BOTH skill lists (`CLAUDE_SKILLS`, `CODEX_SKILLS`). Update the two install tests that assert the exact written list (`install_into_writes_claude_both_then_is_idempotent`, `install_into_writes_codex_openai_yaml_then_is_idempotent`) to include `foreman-kanban`. **Rebuild is what propagates the skill** — note it in the commit body.

- [ ] **Step 4: Gate + commit.**

`cargo test skills_install::` green; `cargo build` (or `--target-dir target/agent` inside foreman) proves the embeds compile. Stage the three skill files + `src/skills_install.rs`. Commit `feat(skills): embedded foreman-kanban skill (tier-2 agent education)`.

---

### Task 6: Feature doc + module map + final gates

**Files:**
- Create: `docs/kanban-board.md`
- Modify: `docs/HANDOFF.md` (§2 module list), `AGENTS.md`/`CLAUDE.md` only if the routing convention demands it (check first)

- [ ] **Step 1: Feature doc.**

`docs/kanban-board.md`, house shape (plain language; no status headers; no counts/line numbers — cite file + symbol): What it does / Why (link the spec for decision history) / How to use it (board window, CLI verbs, dispatch flow, wait) / Gotchas (derived orphans are invisible in the JSON files; close-out on a missing card errors by design; `.foreman/tasks/` travels with the clone — `.gitignore` it per-repo to opt out; skill edits need a rebuild) / **Key files** listing `src/kanban.rs`, `src/board.rs`, the wm seams (`kanban_tick`, `kanban_dispatch`, `drain_board_acts`, `open_board_window`), `src/control.rs` (`KanbanRequest`), `src/skills_install.rs`.

- [ ] **Step 2: Doc plumbing.**

- Add `kanban.rs` and `board.rs` to the `docs/HANDOFF.md` §2 "Architecture / files" list (NOT to CLAUDE.md — it carries no module map by design).
- Check how the existing agent-facing skills are represented: `rg -n "foreman-chat" CLAUDE.md AGENTS.md`. Mirror exactly that for foreman-kanban (expect: a row in AGENTS.md's path-routing table; nothing in CLAUDE.md's dev-skill table). Run `pwsh -File .claude/hooks/cite-guard.ps1 -All` — any output is yours to fix.

- [ ] **Step 3: Final acceptance gates.**

1. `cargo check` clean modulo the known warning baseline.
2. Full `cargo test` green (three consecutive runs if anything PTY-flavored flaked).
3. Ask Andy for a final build-screenshot pass of the board if Task 4's evidence didn't already cover the shipped state.
4. Smoke the CLI against the running debug build: `foreman kanban add "smoke card"`, `list`, `list --json`, `start`/`done` from inside a Session, `wait` exit codes — capture the transcript in the commit body.

- [ ] **Step 4: Commit.**

Stage `docs/kanban-board.md`, `docs/HANDOFF.md`, and any routing-table file actually touched. Commit `docs(kanban): feature doc + module map`.

**Plan deletion happens when the work ships** (per `docs/superpowers/README.md`) — after this plan is fully executed and the feature doc holds the durable content, `git rm` this file. Not before.
