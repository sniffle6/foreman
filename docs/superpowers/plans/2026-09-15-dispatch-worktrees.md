# Dispatch Worktrees Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every card dispatched from the board runs in its own git worktree (`.foreman/worktrees/<id>` on `card/<id>`), the card shows the worktree's state, and close-out integrates and tears it down without ever forcing.

**Architecture:** `kanban.rs` keeps the pure domain (schema, layout naming, prompt template, status parsing, teardown verdict) plus a thin git-subprocess layer; `wm.rs` wires bring-up into `drain_board_acts`, teardown onto `done`/release/`rm`, and drains a channel fed by background threads; `board.rs` paints the badge and the Discard action. Status is derived by a 5 s poll while a board is visible and stored only in memory.

**Tech Stack:** Rust 2024, egui 0.34.3, serde, `std::process::Command` (git), `std::sync::mpsc` + `std::thread` for the background work, `tempfile` in tests.

**Spec:** `docs/superpowers/specs/2026-09-15-dispatch-worktrees-design.md`

## Global Constraints

- Build with `cargo build --target-dir target/agent`; test with `cargo test --target-dir target/agent` (never `--lib`, bin-only crate).
- `Card` files and `list --json` output must be byte-identical for cards without a worktree (`skip_serializing_if = "Option::is_none"`).
- Worktree status is derived, never stored in a card file.
- Teardown never forces except from the human-only Discard action.
- Every git subprocess sets `CREATE_NO_WINDOW` (`0x0800_0000`) on Windows.
- Path stored on the card uses forward slashes: `<root>/.foreman/worktrees/<id>`; branch is `card/<id>`.
- Git-backed tests are skipped (early `return`) when `git` is not on PATH, never failed.
- Commit messages: `type(scope): subject` with the `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` trailer. Never stage `.foreman/`.
- Docs update `docs/kanban-board.md` (one section) and `CONTEXT.md`; no new doc file.

---

### Task 1: The `dispatch_worktrees` setting

**Files:**
- Modify: `src/config.rs` (`struct Settings`, `impl Default`, the `defaults_match_spec` style test)
- Modify: `src/settings_menu.rs` (`enum Field`, `rows(Pane::Agents)`, `adjust`, `display`)

**Interfaces:**
- Produces: `Settings::dispatch_worktrees: bool` (default `true`), read in Task 5 via `crate::config::live(ctx).dispatch_worktrees`.

- [ ] **Step 1: Add the field and default**

In `src/config.rs`, inside `pub struct Settings` right after `install_skills`:

```rust
    /// Dispatch each card into its own git worktree under
    /// `.foreman/worktrees/<id>` (spec: dispatch-worktrees). Off = dispatch
    /// in the project cwd as before. Silently skipped when the cwd is not
    /// inside a git repository.
    pub dispatch_worktrees: bool,
```

In `impl Default for Settings`, after `install_skills: true,`:

```rust
            dispatch_worktrees: true,
```

- [ ] **Step 2: Extend the defaults test**

In the config test that asserts `s.install_skills`, add directly below it:

```rust
        assert!(s.dispatch_worktrees);
```

Also add a serde-compat pin next to the existing serde tests:

```rust
    #[test]
    fn dispatch_worktrees_defaults_on_for_old_settings_files() {
        let s: Settings = serde_json::from_str(r#"{"font_size":13.0}"#).unwrap();
        assert!(s.dispatch_worktrees);
        let off: Settings = serde_json::from_str(r#"{"dispatch_worktrees":false}"#).unwrap();
        assert!(!off.dispatch_worktrees);
    }
```

- [ ] **Step 3: Add the settings-menu row**

In `src/settings_menu.rs`:

`enum Field`: add `DispatchWorktrees,` after `InstallSkills,`.

`rows(Pane::Agents)`: insert after the `InstallSkills` `RowSpec`:

```rust
            RowSpec {
                field: Field::DispatchWorktrees,
                label: "Dispatch cards into git worktrees",
                desc: "Each dispatched card works in .foreman/worktrees/<id> on branch card/<id>",
                kind: Kind::Toggle,
            },
```

`adjust`: add `Field::DispatchWorktrees => flip(&mut s.dispatch_worktrees),` after the `InstallSkills` arm.

`display` (the `to_string` match): add `Field::DispatchWorktrees => s.dispatch_worktrees.to_string(),` after the `InstallSkills` arm.

- [ ] **Step 4: Build and run the config + settings tests**

Run: `cargo test --target-dir target/agent config:: settings_menu::`
Expected: all green (the `every_pane_has_rows_and_labels` test covers the new row).

- [ ] **Step 5: Commit**

```bash
git add src/config.rs src/settings_menu.rs
git commit -m "feat(config): add dispatch_worktrees setting"
```

---

### Task 2: Card schema, layout naming, status parsing, list lines

**Files:**
- Modify: `src/kanban.rs` (`Card`, `CardStore`, `CardLine`, new types + tests)

**Interfaces:**
- Produces:
  - `pub struct Worktree { pub path: String, pub branch: String, pub base: String }` with `pub fn root(&self) -> PathBuf`.
  - `pub fn worktree_layout(root: &Path, id: &str, base: &str) -> Worktree`.
  - `pub struct WorktreeStatus { pub dirty: bool, pub ahead: u32, pub behind: u32, pub missing: bool }` (Copy, Default, Serialize, Deserialize).
  - `pub fn parse_status(porcelain: Option<&str>, rev_list: &str) -> WorktreeStatus`.
  - `pub fn worktree_summary(wt: &Worktree, st: Option<&WorktreeStatus>) -> String` — `card/etxvs5 +3 -1 dirty`.
  - `Card.worktree: Option<Worktree>`.
  - `CardStore::claim_for_dispatch(id, terminal, agent, run, term, worktree: Option<Worktree>)` — `Some` overwrites, `None` leaves the field alone.
  - `CardStore::clear_worktree(&mut self, id) -> Result<(), String>`.
  - `CardStore::worktree_status(&self, id) -> Option<WorktreeStatus>`, `set_worktree_statuses(&mut self, HashMap<String, WorktreeStatus>)`, `take_status_poll(&mut self, now: Instant) -> Option<Vec<(String, Worktree)>>`.
  - `pub const STATUS_POLL_INTERVAL: Duration = 5 s`.
  - `CardLine.worktree_status: Option<WorktreeStatus>`.

- [ ] **Step 1: Write the failing tests**

Append inside `mod tests` in `src/kanban.rs`:

```rust
    #[test]
    fn worktree_layout_names_path_and_branch_with_forward_slashes() {
        let wt = worktree_layout(std::path::Path::new(r"H:\claude code\foreman"), "etxvs5", "main");
        assert_eq!(wt.path, "H:/claude code/foreman/.foreman/worktrees/etxvs5");
        assert_eq!(wt.branch, "card/etxvs5");
        assert_eq!(wt.base, "main");
        assert_eq!(wt.root(), std::path::PathBuf::from("H:/claude code/foreman"));
        // a trailing slash on root does not double up
        let wt = worktree_layout(std::path::Path::new("C:/repo/"), "a1b2c3", "dev");
        assert_eq!(wt.path, "C:/repo/.foreman/worktrees/a1b2c3");
    }

    #[test]
    fn parse_status_reads_porcelain_and_left_right_counts() {
        let s = parse_status(Some(""), "0\t0");
        assert_eq!(s, WorktreeStatus::default());
        let s = parse_status(Some(" M src/wm.rs\n"), "2\t3");
        assert_eq!(s, WorktreeStatus { dirty: true, ahead: 3, behind: 2, missing: false });
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
        }
    }

    #[test]
    fn worktree_summary_renders_branch_counts_and_flags() {
        let wt = sample_worktree();
        assert_eq!(worktree_summary(&wt, None), "card/a3f8k2");
        let st = WorktreeStatus { dirty: false, ahead: 3, behind: 1, missing: false };
        assert_eq!(worktree_summary(&wt, Some(&st)), "card/a3f8k2 +3 -1");
        let st = WorktreeStatus { dirty: true, ..st };
        assert_eq!(worktree_summary(&wt, Some(&st)), "card/a3f8k2 +3 -1 dirty");
        let st = WorktreeStatus { missing: true, ..Default::default() };
        assert_eq!(worktree_summary(&wt, Some(&st)), "card/a3f8k2 +0 -0 missing");
    }

    #[test]
    fn v1_card_file_without_worktree_round_trips_unchanged() {
        let j = r#"{"v":1,"id":"a3f8k2","title":"t","state":"backlog","created":"2026-08-28T13:55:00Z","updated":"2026-08-28T13:55:00Z"}"#;
        let c: Card = serde_json::from_str(j).unwrap();
        assert!(c.worktree.is_none());
        assert_eq!(serde_json::to_string(&c).unwrap(), j);
    }

    #[test]
    fn card_with_worktree_serializes_the_spec_object() {
        let mut c = sample_card(None);
        c.worktree = Some(sample_worktree());
        let s = serde_json::to_string(&c).unwrap();
        assert!(s.contains(r#""worktree":{"path":"H:/repo/.foreman/worktrees/a3f8k2","branch":"card/a3f8k2","base":"main"}"#), "{s}");
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
            .claim_for_dispatch(&id, "t1", "claude", run, TermState::Missing, Some(sample_worktree()))
            .unwrap();
        assert_eq!(store.get(&id).unwrap().worktree, Some(sample_worktree()));
        // done keeps the field: only teardown clears it
        store.done(&id).unwrap();
        assert_eq!(store.get(&id).unwrap().worktree, Some(sample_worktree()));
        // a later claim with None leaves it alone (Restart reuses the tree)
        let id2 = store.add("card two", None).unwrap();
        store
            .claim_for_dispatch(&id2, "t1", "claude", run, TermState::Missing, Some(sample_worktree()))
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
            .claim_for_dispatch(&with, "t1", "claude", run, TermState::Missing, Some(sample_worktree()))
            .unwrap();
        let t0 = std::time::Instant::now();
        // hidden board: nothing
        assert!(store.take_status_poll(t0).is_none());
        store.mark_shown(t0);
        let batch = store.take_status_poll(t0).unwrap();
        assert_eq!(batch, vec![(with.clone(), sample_worktree())]);
        assert!(!batch.iter().any(|(id, _)| id == &plain));
        // in flight: no second batch until results land
        assert!(store.take_status_poll(t0 + STATUS_POLL_INTERVAL * 2).is_none());
        let mut map = std::collections::HashMap::new();
        map.insert(with.clone(), WorktreeStatus { ahead: 2, ..Default::default() });
        store.set_worktree_statuses(map);
        assert_eq!(store.worktree_status(&with).unwrap().ahead, 2);
        // interval not elapsed since the last kick
        assert!(store.take_status_poll(t0 + std::time::Duration::from_secs(1)).is_none());
        store.mark_shown(t0 + STATUS_POLL_INTERVAL * 2);
        assert!(store.take_status_poll(t0 + STATUS_POLL_INTERVAL * 2).is_some());
    }

    #[test]
    fn card_line_carries_worktree_fields_only_when_present() {
        let line = CardLine { card: sample_card(None), orphaned: false, worktree_status: None };
        let j = line.json_line();
        assert!(!j.contains("worktree"), "{j}");
        let mut card = sample_card(None);
        card.worktree = Some(sample_worktree());
        let st = WorktreeStatus { dirty: true, ahead: 3, behind: 1, missing: false };
        let line = CardLine { card, orphaned: false, worktree_status: Some(st) };
        let j = line.json_line();
        assert!(j.contains(r#""worktree":{"#), "{j}");
        assert!(j.contains(r#""worktree_status":{"dirty":true,"ahead":3,"behind":1,"missing":false}"#), "{j}");
        let back: CardLine = serde_json::from_str(&j).unwrap();
        assert_eq!(back, line);
        assert_eq!(
            line.human_line(),
            "a3f8k2  backlog  Fix resize flicker  [wt card/a3f8k2 +3 -1 dirty]"
        );
    }
```

Update every existing `CardLine { card, orphaned }` literal in this file (`card_line_json_round_trips…`, `card_line_human_line…`, `line_with_state`) to add `worktree_status: None`. Do the same in `src/control.rs` tests if any construct `CardLine` (grep `CardLine {`).

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --target-dir target/agent kanban::`
Expected: compile errors (`Worktree` undefined).

- [ ] **Step 3: Implement the types and store changes**

In `src/kanban.rs`, after `POLL_INTERVAL`:

```rust
/// How often a shown board re-derives every worktree card's git status on a
/// background thread (spec: dispatch-worktrees §Status poll).
pub const STATUS_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
```

After `struct Claim`:

```rust
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
}

impl Worktree {
    /// The repository root the layout was derived from: `path` minus its
    /// last three components (`.foreman/worktrees/<id>`).
    pub fn root(&self) -> std::path::PathBuf {
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
```

In `struct Card`, after `claim`:

```rust
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<Worktree>,
```

and `worktree: None,` in `Card::new`.

In `struct CardStore` add:

```rust
    worktree_status: std::collections::HashMap<String, WorktreeStatus>,
    last_status_poll: Option<std::time::Instant>,
    status_inflight: bool,
```

In `impl CardStore`, after `set_orphans`:

```rust
    pub fn worktree_status(&self, id: &str) -> Option<WorktreeStatus> {
        self.worktree_status.get(id).copied()
    }

    /// Replace the derived status map wholesale (one poll round) and release
    /// the in-flight latch so the next round may start.
    pub fn set_worktree_statuses(
        &mut self,
        map: std::collections::HashMap<String, WorktreeStatus>,
    ) {
        self.worktree_status = map;
        self.status_inflight = false;
    }

    /// When a round is due — board shown, no round in flight, interval
    /// elapsed, at least one card has a worktree — return the batch to poll
    /// and latch in-flight. The caller runs git on a background thread and
    /// answers with [`Self::set_worktree_statuses`].
    pub fn take_status_poll(
        &mut self,
        now: std::time::Instant,
    ) -> Option<Vec<(String, Worktree)>> {
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
            return None;
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
```

Change `claim_common` to take `worktree: Option<Worktree>` as its last parameter and, before `card.state = CardState::InProgress;`, add:

```rust
        if let Some(wt) = worktree {
            card.worktree = Some(wt);
        }
```

`start` passes `None`; `claim_for_dispatch` gains `worktree: Option<Worktree>` as its last parameter and forwards it.

`CardLine`: add after `orphaned`:

```rust
    /// Derived from the store's last poll round; absent for cards without a
    /// worktree and for cards not yet polled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_status: Option<WorktreeStatus>,
```

In `human_line`, after the claim push and before the blocked-reason push:

```rust
        if let Some(wt) = &self.card.worktree {
            tail.push(format!(
                "[wt {}]",
                worktree_summary(wt, self.worktree_status.as_ref())
            ));
        }
```

- [ ] **Step 4: Fix the compile fallout and run the tests**

`src/wm.rs` `drain_board_acts` calls `claim_for_dispatch(&id, &term_tag(tid), &agent, run, existing_term)` — append `, None` for now (Task 5 replaces it). `src/wm.rs` `kanban_dispatch` `"list"` arm constructs `CardLine { card, orphaned }` — add `worktree_status: child.kanban.borrow().worktree_status(&c.id)` — but that borrows the store while `store` is already borrowed immutably; both are `borrow()`, which is fine. Write it as `worktree_status: store.worktree_status(&c.id)`.

Run: `cargo test --target-dir target/agent kanban:: control:: wm::kanban`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/kanban.rs src/wm.rs src/control.rs
git commit -m "feat(kanban): worktree schema, layout naming, status parsing, list lines"
```

---

### Task 3: The dispatch prompt's Workspace section

**Files:**
- Modify: `src/kanban.rs` (`dispatch_prompt` + tests)

**Interfaces:**
- Consumes: `Card.worktree`, `Worktree::root`.
- Produces: `dispatch_prompt(card, style)` unchanged signature; with a worktree it renders the spec template.

- [ ] **Step 1: Write the failing tests**

```rust
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
             Integrate first, from inside your worktree:\n\
             \x20   git rebase main\n\
             \x20   git -C \"H:/repo\" merge --ff-only card/a3f8k2\n\
             Resolve rebase conflicts yourself. If the fast-forward is refused, rebase again and retry.\n\
             If git refuses because the main checkout has uncommitted changes in files you touched, block instead of forcing.\n\
             When the work is complete, run:    foreman kanban done a3f8k2\n\
             If you are stuck and need a human: foreman kanban block a3f8k2 --reason \"<one line>\"\n\
             Do not end the session without running one of these."
        );
    }

    #[test]
    fn dispatch_prompt_with_worktree_envvar_style_keeps_git_lines_style_independent() {
        let mut card = sample_card(None);
        card.worktree = Some(sample_worktree());
        let s = dispatch_prompt(&card, CloseoutStyle::EnvVar);
        assert!(s.contains("# Workspace\n"));
        assert!(s.contains("    git rebase main\n    git -C \"H:/repo\" merge --ff-only card/a3f8k2\n"));
        assert!(s.contains("When the work is complete, run:    & $env:FOREMAN_EXE kanban done a3f8k2\n"));
        assert!(s.contains("(bash: write \"$FOREMAN_EXE\" in place of & $env:FOREMAN_EXE)\n"));
    }
```

(The `\x20   ` escapes keep the four leading spaces of the git lines through the `\` line continuation, which strips leading whitespace.)

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --target-dir target/agent kanban::dispatch_prompt_with_worktree`
Expected: FAIL (assertion, today's text has no Workspace section).

- [ ] **Step 3: Implement**

Replace the body of `dispatch_prompt`:

```rust
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
    if let Some(wt) = &card.worktree {
        let root = wt.root();
        out.push_str(&format!(
            "# Workspace\n\
             You are in a git worktree at {path}, on branch {branch}, based on {base}.\n\
             The main checkout at {root} is shared with other workers: never edit files there.\n\
             Leave .foreman/ untouched and never stage it.\n\
             \n",
            path = wt.path,
            branch = wt.branch,
            base = wt.base,
            root = root.display(),
        ));
    }
    out.push_str("# Close-out (required)\n");
    if let Some(wt) = &card.worktree {
        out.push_str(&format!(
            "Integrate first, from inside your worktree:\n\
             \x20   git rebase {base}\n\
             \x20   git -C \"{root}\" merge --ff-only {branch}\n\
             Resolve rebase conflicts yourself. If the fast-forward is refused, rebase again and retry.\n\
             If git refuses because the main checkout has uncommitted changes in files you touched, block instead of forcing.\n",
            base = wt.base,
            root = wt.root().display(),
            branch = wt.branch,
        ));
    }
    let closeout = match style {
        CloseoutStyle::Path => format!(
            "When the work is complete, run:    foreman kanban done {id}\n\
             If you are stuck and need a human: foreman kanban block {id} --reason \"<one line>\"\n\
             Do not end the session without running one of these.",
            id = card.id,
        ),
        CloseoutStyle::EnvVar => format!(
            "When the work is complete, run:    & $env:FOREMAN_EXE kanban done {id}\n\
             If you are stuck and need a human: & $env:FOREMAN_EXE kanban block {id} --reason \"<one line>\"\n\
             (bash: write \"$FOREMAN_EXE\" in place of & $env:FOREMAN_EXE)\n\
             Do not end the session without running one of these.",
            id = card.id,
        ),
    };
    out + &closeout
}
```

- [ ] **Step 4: Run all four existing prompt tests plus the two new ones**

Run: `cargo test --target-dir target/agent kanban::dispatch_prompt`
Expected: PASS (the four verbatim tests for cards without a worktree must still be byte-identical).

- [ ] **Step 5: Commit**

```bash
git add src/kanban.rs
git commit -m "feat(kanban): dispatch prompt gains the worktree Workspace section"
```

---

### Task 4: Git plumbing — bring-up, status, teardown

**Files:**
- Modify: `src/kanban.rs` (new `git` section + integration tests)

**Interfaces:**
- Produces:
  - `pub fn git_available() -> bool`.
  - `pub enum BringUp { Worktree(Worktree), InPlace, Detached }`.
  - `pub fn bring_up_worktree(project_cwd: &Path, card: &Card) -> Result<BringUp, String>`.
  - `pub fn worktree_status_now(project_cwd: &Path, wt: &Worktree) -> WorktreeStatus` (the live probe; the poll and the `rm` pre-check both use it).
  - `pub enum TeardownOutcome { Removed, Dirty, Unmerged { ahead: u32 }, Failed(String) }`.
  - `pub fn teardown_verdict(remove: Result<(), String>, branch: Option<Result<(), String>>, ahead: u32) -> TeardownOutcome`.
  - `pub fn teardown_worktree(project_cwd: &Path, wt: &Worktree, force: bool) -> TeardownOutcome`.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn teardown_verdict_covers_the_four_row_table() {
        let dirty = Err("fatal: 'x' contains modified or untracked files, use --force to delete it".to_string());
        let unmerged = Err("error: the branch 'card/a' is not fully merged".to_string());
        let other = Err("fatal: something else".to_string());
        assert_eq!(teardown_verdict(Ok(()), Some(Ok(())), 0), TeardownOutcome::Removed);
        assert_eq!(teardown_verdict(dirty.clone(), None, 0), TeardownOutcome::Dirty);
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
        assert_eq!(teardown_verdict(dirty, Some(Ok(())), 0), TeardownOutcome::Dirty);
        assert!(matches!(teardown_verdict(Ok(()), None, 0), TeardownOutcome::Failed(_)));
    }

    /// `git init -b main` + one tracked file, identity passed inline so the
    /// test never depends on the machine's git config.
    fn git_repo() -> Option<tempfile::TempDir> {
        if !git_available() {
            eprintln!("git not on PATH; skipping");
            return None;
        }
        let tmp = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(tmp.path())
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        run(&["init", "-q", "-b", "main"]);
        std::fs::write(tmp.path().join("f.txt"), "one\n").unwrap();
        run(&["add", "f.txt"]);
        run(&["commit", "-q", "-m", "init"]);
        Some(tmp)
    }

    fn git_in(dir: &std::path::Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
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
        assert!(wt.path.ends_with("/.foreman/worktrees/a1b2c3"), "{}", wt.path);
        assert!(std::path::Path::new(&wt.path).join("f.txt").exists());
        let listed = git_in(repo.path(), &["worktree", "list", "--porcelain"]);
        assert!(listed.to_lowercase().contains(&wt.path.to_lowercase()), "{listed}");
        assert_eq!(git_in(std::path::Path::new(&wt.path), &["symbolic-ref", "--short", "HEAD"]), "card/a1b2c3");
        // ignored locally via info/exclude, never via .gitignore
        assert!(!repo.path().join(".gitignore").exists());
        let status = git_in(repo.path(), &["status", "--porcelain"]);
        assert!(status.is_empty(), "worktree dir must be ignored: {status}");
        // status probe: clean, 0/0
        assert_eq!(worktree_status_now(repo.path(), &wt), WorktreeStatus::default());
    }

    #[test]
    fn bring_up_is_in_place_outside_a_repo_and_detached_without_a_branch() {
        if !git_available() { return }
        let tmp = tempfile::tempdir().unwrap();
        let card = Card::new("a1b2c3".into(), "t".into(), None, now_stamp());
        assert!(matches!(bring_up_worktree(tmp.path(), &card).unwrap(), BringUp::InPlace));
        let Some(repo) = git_repo() else { return };
        let head = git_in(repo.path(), &["rev-parse", "HEAD"]);
        git_in(repo.path(), &["checkout", "-q", "--detach", &head]);
        assert!(matches!(bring_up_worktree(repo.path(), &card).unwrap(), BringUp::Detached));
    }

    #[test]
    fn bring_up_reuses_an_existing_tree_and_readds_a_leftover_branch() {
        let Some(repo) = git_repo() else { return };
        let mut card = Card::new("a1b2c3".into(), "t".into(), None, now_stamp());
        let BringUp::Worktree(wt) = bring_up_worktree(repo.path(), &card).unwrap() else { panic!() };
        card.worktree = Some(wt.clone());
        // Restart: same tree, uncommitted state intact
        std::fs::write(std::path::Path::new(&wt.path).join("f.txt"), "edited\n").unwrap();
        let BringUp::Worktree(again) = bring_up_worktree(repo.path(), &card).unwrap() else { panic!() };
        assert_eq!(again, wt);
        assert_eq!(std::fs::read_to_string(std::path::Path::new(&wt.path).join("f.txt")).unwrap(), "edited\n");
        assert!(worktree_status_now(repo.path(), &wt).dirty);
        // branch left behind by an unmerged teardown: re-add on the branch
        git_in(std::path::Path::new(&wt.path), &["checkout", "-q", "--", "f.txt"]);
        git_in(repo.path(), &["worktree", "remove", &wt.path]);
        assert!(!std::path::Path::new(&wt.path).exists());
        let BringUp::Worktree(third) = bring_up_worktree(repo.path(), &card).unwrap() else { panic!() };
        assert_eq!(third.path, wt.path);
        assert!(std::path::Path::new(&wt.path).exists());
        assert_eq!(git_in(std::path::Path::new(&wt.path), &["symbolic-ref", "--short", "HEAD"]), "card/a1b2c3");
    }

    #[test]
    fn teardown_after_merge_leaves_nothing() {
        let Some(repo) = git_repo() else { return };
        let card = Card::new("a1b2c3".into(), "t".into(), None, now_stamp());
        let BringUp::Worktree(wt) = bring_up_worktree(repo.path(), &card).unwrap() else { panic!() };
        let tree = std::path::Path::new(&wt.path);
        std::fs::write(tree.join("f.txt"), "two\n").unwrap();
        git_in(tree, &["commit", "-q", "-am", "work"]);
        assert_eq!(worktree_status_now(repo.path(), &wt).ahead, 1);
        git_in(repo.path(), &["merge", "--ff-only", "card/a1b2c3"]);
        assert_eq!(teardown_worktree(repo.path(), &wt, false), TeardownOutcome::Removed);
        assert!(!tree.exists());
        assert!(git_in(repo.path(), &["branch", "--list", "card/a1b2c3"]).is_empty());
    }

    #[test]
    fn teardown_keeps_a_dirty_tree() {
        let Some(repo) = git_repo() else { return };
        let card = Card::new("a1b2c3".into(), "t".into(), None, now_stamp());
        let BringUp::Worktree(wt) = bring_up_worktree(repo.path(), &card).unwrap() else { panic!() };
        std::fs::write(std::path::Path::new(&wt.path).join("f.txt"), "two\n").unwrap();
        assert_eq!(teardown_worktree(repo.path(), &wt, false), TeardownOutcome::Dirty);
        assert!(std::path::Path::new(&wt.path).exists());
        assert!(!git_in(repo.path(), &["branch", "--list", "card/a1b2c3"]).is_empty());
        // Discard is the only forcing path
        assert_eq!(teardown_worktree(repo.path(), &wt, true), TeardownOutcome::Removed);
        assert!(!std::path::Path::new(&wt.path).exists());
        assert!(git_in(repo.path(), &["branch", "--list", "card/a1b2c3"]).is_empty());
    }

    #[test]
    fn teardown_keeps_an_unmerged_branch_and_reports_the_count() {
        let Some(repo) = git_repo() else { return };
        let card = Card::new("a1b2c3".into(), "t".into(), None, now_stamp());
        let BringUp::Worktree(wt) = bring_up_worktree(repo.path(), &card).unwrap() else { panic!() };
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
        let st = worktree_status_now(repo.path(), &wt);
        assert!(st.missing);
        assert_eq!(st.ahead, 2);
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --target-dir target/agent kanban::teardown kanban::bring_up`
Expected: compile errors.

- [ ] **Step 3: Implement the git section**

Add before `#[cfg(test)]` in `src/kanban.rs`:

```rust
// ---------------------------------------------------------------------------
// Git plumbing for per-card worktrees (spec: dispatch-worktrees). Every call
// is a short-lived `git` subprocess with no console window. Bring-up runs
// synchronously in the dispatch drain; teardown and the status poll run on
// background threads (wm.rs owns the threads and the channel).
// ---------------------------------------------------------------------------

/// Run `git -C <cwd> <args>`; `Ok(stdout trimmed)` on exit 0, otherwise
/// `Err(first stderr line)` (or a spawn error). Never opens a console window.
fn git(cwd: &std::path::Path, args: &[&str]) -> Result<String, String> {
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
        let first = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
        if first.is_empty() {
            Err(format!("git {} failed ({})", args.first().unwrap_or(&""), out.status))
        } else {
            Err(first.to_string())
        }
    }
}

/// True when `git` answers `--version` — the skip gate for git-backed tests
/// and the "not installed" branch of every caller.
pub fn git_available() -> bool {
    git(std::path::Path::new("."), &["--version"]).is_ok()
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

/// Case-insensitive on Windows (drive letters and user dirs vary in case
/// between `rev-parse` and the porcelain listing), exact elsewhere.
fn same_path(a: &str, b: &str) -> bool {
    let a = a.replace('\\', "/");
    let b = b.replace('\\', "/");
    if cfg!(windows) {
        a.eq_ignore_ascii_case(&b)
    } else {
        a == b
    }
}

/// Spec §Bring-up, steps 1–5. Any git failure is `Err` with git's first
/// stderr line; the caller aborts before spawning.
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
    // 3. naming; a card that already carries a worktree keeps its recorded base
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
        let exclude = if exclude.is_absolute() { exclude } else { root.join(exclude) };
        if let Some(parent) = exclude.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        let mut text = std::fs::read_to_string(&exclude).unwrap_or_default();
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(".foreman/worktrees/\n");
        std::fs::write(&exclude, text).map_err(|e| format!("cannot write {}: {e}", exclude.display()))?;
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
        git(&root, &["worktree", "add", "-b", &wt.branch, &wt.path, "HEAD"])?;
    }
    Ok(BringUp::Worktree(wt))
}

/// Live status probe (spec §Status poll): dirty from inside the tree,
/// ahead/behind from any checkout of the repo. Also the synchronous `rm`
/// pre-check. A vanished directory reads as `missing` with counts intact.
pub fn worktree_status_now(project_cwd: &std::path::Path, wt: &Worktree) -> WorktreeStatus {
    let tree = std::path::Path::new(&wt.path);
    let porcelain = if tree.is_dir() {
        Some(git(tree, &["status", "--porcelain", "--untracked-files=no"]).unwrap_or_default())
    } else {
        None
    };
    let range = format!("{}...{}", wt.base, wt.branch);
    let rev_list = git(project_cwd, &["rev-list", "--left-right", "--count", &range]).unwrap_or_default();
    parse_status(porcelain.as_deref(), &rev_list)
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
/// human-only Discard path (`remove --force`, `branch -D`). A directory that
/// is already gone is pruned instead of removed so the branch is no longer
/// "checked out" and `branch -d` can judge it.
pub fn teardown_worktree(
    project_cwd: &std::path::Path,
    wt: &Worktree,
    force: bool,
) -> TeardownOutcome {
    let ahead = worktree_status_now(project_cwd, wt).ahead;
    let remove = if std::path::Path::new(&wt.path).is_dir() {
        let args: &[&str] = if force {
            &["worktree", "remove", "--force", &wt.path]
        } else {
            &["worktree", "remove", &wt.path]
        };
        git(project_cwd, args).map(|_| ())
    } else {
        git(project_cwd, &["worktree", "prune"]).map(|_| ())
    };
    let branch = remove.is_ok().then(|| {
        let flag = if force { "-D" } else { "-d" };
        git(project_cwd, &["branch", flag, &wt.branch]).map(|_| ())
    });
    let _ = git(project_cwd, &["worktree", "prune"]);
    teardown_verdict(remove, branch, ahead)
}
```

- [ ] **Step 4: Run the git-backed and verdict tests**

Run: `cargo test --target-dir target/agent kanban::teardown kanban::bring_up`
Expected: PASS. If `worktree remove` reports a different wording on this git (2.39), read the message from the failing assertion and widen the `contains` check in `teardown_verdict`; do not add `--force`.

- [ ] **Step 5: Commit**

```bash
git add src/kanban.rs
git commit -m "feat(kanban): git bring-up, status probe, and non-forcing teardown for card worktrees"
```

---

### Task 5: Wire bring-up, teardown, poll, and Discard into the window manager

**Files:**
- Modify: `src/wm.rs` (`WindowManager` fields + `new`, `CloseTarget`, `resolve_pending`, `kanban_dispatch`, `drain_board_acts`, the `show` drain call, `kanban_dispatch` test call sites)
- Modify: `src/board.rs` (`BoardAct::DiscardWorktree` only; the button lands in Task 6)

**Interfaces:**
- Consumes: everything from Tasks 2–4, `Settings::dispatch_worktrees`.
- Produces: `BoardAct::DiscardWorktree(String)`; `WindowManager::drain_worktree_msgs(&mut self, ctx)`; `kanban_dispatch(&mut self, req, ctx)`; `resolve_pending(&mut self, outcome, ctx)`.

- [ ] **Step 1: Write the failing wm tests**

Find the existing `wm::` kanban dispatch test that drives `BoardAct::Dispatch` (grep `BoardAct::Dispatch` in `src/wm.rs` tests) and copy its project setup. Add:

```rust
    /// Drain the worktree channel until `pred` holds or 10 s pass — teardown
    /// runs on a thread, so the test polls the same seam the frame loop does.
    fn drain_until(m: &mut WindowManager, ctx: &egui::Context, mut pred: impl FnMut(&WindowManager) -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !pred(m) {
            assert!(std::time::Instant::now() < deadline, "timed out waiting on worktree thread");
            m.drain_worktree_msgs(ctx);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    fn dispatch_creates_a_worktree_and_done_tears_it_down() {
        if !crate::kanban::git_available() { return }
        let Some(repo) = kanban_git_repo() else { return };
        let ctx = egui::Context::default();
        let mut settings = crate::config::Settings::default();
        settings.dispatch_worktrees = true;
        crate::config::seed_live(&ctx, &settings);
        let mut m = WindowManager::new();
        m.cwd = Some(repo.path().to_path_buf());
        m.tag = Some("p1".into());
        m.kanban.borrow_mut().set_dir(Some(repo.path()));
        let id = m.kanban.borrow_mut().add("card", None).unwrap();
        m.open_board_window();
        board_of(&mut m).acts.push(crate::board::BoardAct::Dispatch { id: id.clone(), agent: pause_argv()[0].clone() });
        m.drain_board_acts(&ctx);
        let wt = m.kanban.borrow().get(&id).unwrap().worktree.clone().expect("worktree recorded");
        assert_eq!(wt.branch, format!("card/{id}"));
        assert!(std::path::Path::new(&wt.path).is_dir());
        // the worker's cwd is the worktree
        let tid = m.kanban.borrow().get(&id).unwrap().claim.as_ref().unwrap().terminal.clone();
        assert!(m.terminal_cwd(&tid).is_some_and(|c| crate::kanban::same_path_pub(&c.to_string_lossy(), &wt.path)));
        // done → teardown thread → field cleared
        board_of(&mut m).acts.push(crate::board::BoardAct::Done(id.clone()));
        m.drain_board_acts(&ctx);
        drain_until(&mut m, &ctx, |m| m.kanban.borrow().get(&id).unwrap().worktree.is_none());
        assert!(!std::path::Path::new(&wt.path).exists());
    }

    #[test]
    fn dispatch_with_the_setting_off_records_no_worktree() {
        if !crate::kanban::git_available() { return }
        let Some(repo) = kanban_git_repo() else { return };
        let ctx = egui::Context::default();
        let mut settings = crate::config::Settings::default();
        settings.dispatch_worktrees = false;
        crate::config::seed_live(&ctx, &settings);
        let mut m = WindowManager::new();
        m.cwd = Some(repo.path().to_path_buf());
        m.tag = Some("p1".into());
        m.kanban.borrow_mut().set_dir(Some(repo.path()));
        let id = m.kanban.borrow_mut().add("card", None).unwrap();
        m.open_board_window();
        board_of(&mut m).acts.push(crate::board::BoardAct::Dispatch { id: id.clone(), agent: pause_argv()[0].clone() });
        m.drain_board_acts(&ctx);
        assert!(m.kanban.borrow().get(&id).unwrap().claim.is_some());
        assert!(m.kanban.borrow().get(&id).unwrap().worktree.is_none());
        assert!(!repo.path().join(".foreman").join("worktrees").exists());
    }

    #[test]
    fn rm_refuses_a_card_whose_worktree_has_unmerged_work() {
        if !crate::kanban::git_available() { return }
        let Some(repo) = kanban_git_repo() else { return };
        let ctx = egui::Context::default();
        crate::config::seed_live(&ctx, &crate::config::Settings::default());
        let mut m = WindowManager::new();
        m.cwd = Some(repo.path().to_path_buf());
        m.tag = Some("p1".into());
        m.kanban.borrow_mut().set_dir(Some(repo.path()));
        let id = m.kanban.borrow_mut().add("card", None).unwrap();
        m.open_board_window();
        board_of(&mut m).acts.push(crate::board::BoardAct::Dispatch { id: id.clone(), agent: pause_argv()[0].clone() });
        m.drain_board_acts(&ctx);
        let wt = m.kanban.borrow().get(&id).unwrap().worktree.clone().unwrap();
        std::fs::write(std::path::Path::new(&wt.path).join("f.txt"), "edit\n").unwrap();
        let rm = crate::control::KanbanRequest { cmd: "kanban".into(), action: "rm".into(), id: Some(id.clone()), project: Some("p1".into()), ..Default::default() };
        let e = m.kanban_dispatch(&rm, &ctx).unwrap_err();
        assert!(e.contains("unmerged work"), "{e}");
        assert!(m.kanban.borrow().get(&id).is_some());
    }
```

Helpers the tests need in the wm test module: `kanban_git_repo()` (same body as Task 4's `git_repo`, duplicated here because test modules are private to their file), `board_of(&mut m) -> &mut crate::board::BoardView` (find the `Content::Board` tab), and a `WindowManager::terminal_cwd(&self, tag: &str) -> Option<PathBuf>` test-only accessor if `Session` exposes its cwd (grep `fn cwd` in `src/terminal.rs`; if it does not exist, drop that one assertion and instead assert `dispatch_prompt` text: check the spawned tab title equals the card title and skip cwd). Replace `crate::kanban::same_path_pub` with a `pub(crate) fn same_path` (make Task 4's `same_path` `pub(crate)`).

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --target-dir target/agent wm::dispatch_creates_a_worktree`
Expected: compile errors (`drain_worktree_msgs`, `kanban_dispatch` arity).

- [ ] **Step 3: Implement the wm wiring**

**3a. Channel + fields.** Above `pub struct WindowManager`:

```rust
/// Results from the worktree background threads (teardown, status poll),
/// drained once per frame by `drain_worktree_msgs`.
enum WorktreeMsg {
    Teardown { id: String, outcome: crate::kanban::TeardownOutcome },
    Status(std::collections::HashMap<String, crate::kanban::WorktreeStatus>),
}
```

Fields on `WindowManager` (after `kanban`):

```rust
    /// Worktree thread results (see `WorktreeMsg`). The sender is cloned
    /// into each spawned thread; the receiver is drained per frame.
    worktree_tx: std::sync::mpsc::Sender<WorktreeMsg>,
    worktree_rx: std::sync::mpsc::Receiver<WorktreeMsg>,
```

In `new()`: `let (worktree_tx, worktree_rx) = std::sync::mpsc::channel();` before `Self { … }`, then `worktree_tx, worktree_rx,` in the literal.

**3b. `CloseTarget`** gains `DiscardWorktree(String)` (card id). In `resolve_pending`, add a `ctx: &egui::Context` parameter and the arm:

```rust
                    CloseTarget::DiscardWorktree(id) => {
                        let wt = self.kanban.borrow().get(&id).and_then(|c| c.worktree.clone());
                        if let Some(wt) = wt {
                            self.start_teardown(&id, wt, true, ctx);
                        }
                    }
```

Update the call in `show` (`self.resolve_pending(outcome)` → `self.resolve_pending(outcome, &ctx)`) and any test call sites (`grep -n "resolve_pending(" src/wm.rs`) with `&egui::Context::default()`.

**3c. Helpers**, placed after `drain_board_acts`:

```rust
    /// Spawn the non-forcing (or, for Discard, forcing) teardown on a thread;
    /// the result lands in `worktree_rx` and is applied by
    /// `drain_worktree_msgs`. Removing a tree that holds a `target/` dir
    /// deletes gigabytes — never on the UI thread.
    fn start_teardown(
        &self,
        id: &str,
        wt: crate::kanban::Worktree,
        force: bool,
        ctx: &egui::Context,
    ) {
        let Some(cwd) = self.cwd.clone() else { return };
        let tx = self.worktree_tx.clone();
        let ctx = ctx.clone();
        let id = id.to_string();
        std::thread::spawn(move || {
            let outcome = crate::kanban::teardown_worktree(&cwd, &wt, force);
            let _ = tx.send(WorktreeMsg::Teardown { id, outcome });
            ctx.request_repaint();
        });
    }

    /// Teardown for `done` / Release: only if the card carries a worktree.
    fn teardown_if_worktree(&self, id: &str, ctx: &egui::Context) {
        let wt = self.kanban.borrow().get(id).and_then(|c| c.worktree.clone());
        if let Some(wt) = wt {
            self.start_teardown(id, wt, false, ctx);
        }
    }

    /// `rm` pre-check + delete + teardown (spec §Teardown, the `rm` row):
    /// refuse while the tree is dirty or ahead of base, so a deleted card
    /// never orphans a branch nobody can find.
    fn kanban_rm(&mut self, id: &str, ctx: &egui::Context) -> Result<(), String> {
        let wt = self.kanban.borrow().get(id).and_then(|c| c.worktree.clone());
        if let (Some(wt), Some(cwd)) = (&wt, self.cwd.as_deref()) {
            let st = crate::kanban::worktree_status_now(cwd, wt);
            if st.dirty || st.ahead > 0 {
                return Err(format!(
                    "card {id} has unmerged work in its worktree; discard it first"
                ));
            }
        }
        self.kanban.borrow_mut().rm(id)?;
        if let Some(wt) = wt {
            self.start_teardown(id, wt, false, ctx);
        }
        Ok(())
    }

    /// Apply worktree thread results and kick the status poll when due.
    /// Runs every frame from `show`, per manager (each project owns its
    /// threads' channel, like its store).
    fn drain_worktree_msgs(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.worktree_rx.try_recv() {
            match msg {
                WorktreeMsg::Status(map) => self.kanban.borrow_mut().set_worktree_statuses(map),
                WorktreeMsg::Teardown { id, outcome } => {
                    use crate::kanban::TeardownOutcome::*;
                    match outcome {
                        Removed => {
                            // An rm'd card has nothing to clear — not an error.
                            let _ = self.kanban.borrow_mut().clear_worktree(&id);
                        }
                        Dirty => crate::notify::queue(
                            ctx,
                            crate::notify::Level::Warning,
                            format!("card {id}: worktree kept: uncommitted changes"),
                        ),
                        Unmerged { ahead } => {
                            let base = self
                                .kanban
                                .borrow()
                                .get(&id)
                                .and_then(|c| c.worktree.as_ref().map(|w| w.base.clone()))
                                .unwrap_or_else(|| "base".into());
                            crate::notify::queue(
                                ctx,
                                crate::notify::Level::Warning,
                                format!("card {id}: branch kept: {ahead} commits not on {base}"),
                            );
                        }
                        Failed(e) => crate::notify::queue(
                            ctx,
                            crate::notify::Level::Error,
                            format!("card {id}: worktree teardown failed: {e}"),
                        ),
                    }
                }
            }
        }
        let now = std::time::Instant::now();
        let batch = self.kanban.borrow_mut().take_status_poll(now);
        if let (Some(batch), Some(cwd)) = (batch, self.cwd.clone()) {
            let tx = self.worktree_tx.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let map = batch
                    .iter()
                    .map(|(id, wt)| (id.clone(), crate::kanban::worktree_status_now(&cwd, wt)))
                    .collect();
                let _ = tx.send(WorktreeMsg::Status(map));
                ctx.request_repaint();
            });
        }
    }
```

In `show`, right after `self.drain_board_acts(&ctx);` add `self.drain_worktree_msgs(&ctx);`.

**3d. `kanban_dispatch(&mut self, req, ctx: &egui::Context)`.** The `"done"` arm: after `child.kanban.borrow_mut().done(id)?;` add `child.teardown_if_worktree(id, ctx);`. The `"rm"` arm: replace the store call with `child.kanban_rm(id, ctx)?;`. The `"list"` arm sets `worktree_status: store.worktree_status(&c.id)` (done in Task 2). Update the one production call (`self.kanban_dispatch(&req)` → `self.kanban_dispatch(&req, ctx)`) and every test call (`sed -i 's/kanban_dispatch(&\([a-z0-9_]*\))/kanban_dispatch(\&\1, \&ctx)/g'` then add `let ctx = egui::Context::default();` where missing).

**3e. `drain_board_acts`:**

- `Done(id)`: on `Ok` → `self.teardown_if_worktree(&id, ctx);`.
- `Release(id)`: on `Ok` → `self.teardown_if_worktree(&id, ctx);`.
- `Rm(id)`: replace `self.kanban.borrow_mut().rm(&id)` with `self.kanban_rm(&id, ctx)`.
- New arm:

```rust
                crate::board::BoardAct::DiscardWorktree(id) => {
                    if self.overlay_blocks_close() {
                        continue;
                    }
                    let Some(wt) = self.kanban.borrow().get(&id).and_then(|c| c.worktree.clone()) else {
                        continue;
                    };
                    self.pending_close = Some(PendingClose {
                        target: CloseTarget::DiscardWorktree(id),
                        view: crate::confirm::ConfirmClose::new(
                            "Discard worktree?",
                            format!(
                                "Deletes {} and branch {}, including any uncommitted or unmerged work.",
                                wt.path, wt.branch
                            ),
                            "Discard",
                            Vec::new(),
                        ),
                    });
                }
```

- `Dispatch { id, agent }`: replace the body's head with:

```rust
                    let Some(mut card) = self.kanban.borrow().get(&id).cloned() else { /* unchanged error toast */ };
                    let mut record: Option<crate::kanban::Worktree> = None;
                    if crate::config::live(ctx).dispatch_worktrees {
                        if let Some(cwd) = self.cwd.clone() {
                            match crate::kanban::bring_up_worktree(&cwd, &card) {
                                Ok(crate::kanban::BringUp::Worktree(wt)) => {
                                    card.worktree = Some(wt.clone());
                                    record = Some(wt);
                                }
                                Ok(crate::kanban::BringUp::InPlace) => {}
                                Ok(crate::kanban::BringUp::Detached) => {
                                    card.worktree = None;
                                    crate::notify::queue(
                                        ctx,
                                        crate::notify::Level::Warning,
                                        format!("card {id}: no branch checked out; dispatched without a worktree"),
                                    );
                                }
                                Err(e) => {
                                    crate::notify::queue(
                                        ctx,
                                        crate::notify::Level::Error,
                                        format!("card {id}: worktree bring-up failed: {e}"),
                                    );
                                    continue;
                                }
                            }
                        }
                    } else {
                        card.worktree = None; // prompt renders today's text
                    }
                    let prompt = crate::kanban::dispatch_prompt(&card, crate::kanban::closeout_style());
                    let spawn_cwd = record.as_ref().map(|w| std::path::PathBuf::from(&w.path));
                    match self.add_terminal_cmd(&[agent.clone(), prompt], spawn_cwd.as_deref(), Some(&card.title), ctx) {
```

and pass `record` as the new last argument of `claim_for_dispatch`. Note `card.worktree = None` for the setting-off / Detached paths affects only the prompt: `record` is `None` there, so `claim_common` leaves any stored field untouched.

- [ ] **Step 4: Build, run the wm and control tests**

Run: `cargo test --target-dir target/agent wm:: control:: kanban::`
Expected: PASS, including the three new wm tests.

- [ ] **Step 5: Commit**

```bash
git add src/wm.rs src/board.rs
git commit -m "feat(wm): dispatch into per-card worktrees; teardown on done/release/rm; status poll; Discard confirm"
```

---

### Task 6: Board surfaces — card face line, detail fields, Discard

**Files:**
- Modify: `src/board.rs` (`show_card`, `show_details`, tests)

**Interfaces:**
- Consumes: `Card.worktree`, `CardStore::worktree_status`, `kanban::worktree_summary`, `BoardAct::DiscardWorktree`.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn detail_page_offers_discard_only_for_done_blocked_or_orphaned_worktree_cards() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_at(tmp.path());
        let id = store.borrow_mut().add("card", None).unwrap();
        let wt = crate::kanban::Worktree {
            path: "H:/repo/.foreman/worktrees/x".into(),
            branch: "card/x".into(),
            base: "main".into(),
        };
        store
            .borrow_mut()
            .claim_for_dispatch(&id, "t1", "claude", crate::kanban::run_nonce(), crate::kanban::TermState::Missing, Some(wt))
            .unwrap();
        let mut board = BoardView::new(Rc::clone(&store));
        board.selected = Some(id.clone());
        let ctx = egui::Context::default();
        let base = egui::Id::new("discard");
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        // In Progress with a live claim: no Discard button rendered.
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert!(!board.offered_discard);
        store.borrow_mut().done(&id).unwrap();
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert!(board.offered_discard, "Done + worktree must offer Discard");
        // The card face renders the summary line without a status yet.
        board.selected = None;
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert!(board.acts.is_empty());
    }
```

Add a `#[cfg(test)] pub(crate) offered_discard: bool` field on `BoardView` (default `false`, set each frame by `show_details` when the button is drawn) — the same style as the existing `store()` test accessor. Keep the field test-only so production carries no dead state.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --target-dir target/agent board::detail_page_offers_discard`
Expected: compile error (`offered_discard` missing).

- [ ] **Step 3: Implement the card face line**

In `show_card`, change the status galley so a card with a worktree keeps one row for the status and one for the worktree line. Replace the block from `let mut job = egui::text::LayoutJob::simple(status, …)` through its `cp.galley(…)` with:

```rust
        let has_wt = card.worktree.is_some();
        let mut job = egui::text::LayoutJob::simple(
            status,
            egui::FontId::proportional(10.5 * self.scale),
            status_color,
            text_w,
        );
        job.wrap.max_rows = if has_wt { 1 } else { 2 };
        job.wrap.break_anywhere = false;
        cp.galley(
            card_rect.min + egui::vec2(PAD * self.scale, 40.0 * self.scale),
            cp.layout_job(job),
            status_color,
        );
        if let Some(wt) = &card.worktree {
            let st = self.store.borrow().worktree_status(&card.id);
            let attention = st.is_some_and(|s| {
                s.dirty || (s.ahead > 0 && card.state != crate::kanban::CardState::InProgress)
            });
            let color = if st.is_some_and(|s| s.missing) {
                th.dim
            } else if attention {
                th.danger
            } else {
                th.text
            };
            let mut job = egui::text::LayoutJob::simple(
                crate::kanban::worktree_summary(wt, st.as_ref()),
                egui::FontId::monospace(10.0 * self.scale),
                color,
                text_w,
            );
            job.wrap.max_rows = 1;
            job.wrap.break_anywhere = true;
            cp.galley(
                card_rect.min + egui::vec2(PAD * self.scale, 54.0 * self.scale),
                cp.layout_job(job),
                color,
            );
        }
```

Add the test-only field to `BoardView`:

```rust
    #[cfg(test)]
    pub(crate) offered_discard: bool,
```

initialised `offered_discard: false,` in `new` under `#[cfg(test)]`.

- [ ] **Step 4: Implement the detail-page fields and Discard**

In `show_details`, after the claim block (`if let Some(claim) = &card.claim { … }`) insert:

```rust
                if let Some(wt) = &card.worktree {
                    ui.add_space(12.0 * self.scale);
                    ui.strong("Worktree");
                    ui.add(egui::Label::new(format!("Path: {}", wt.path)).wrap().selectable(true));
                    ui.label(format!("Branch: {}   base: {}", wt.branch, wt.base));
                    let st = self.store.borrow().worktree_status(&card.id);
                    let line = match st {
                        None => "Status: not polled yet".to_owned(),
                        Some(s) if s.missing => format!("Status: directory missing · +{} -{}", s.ahead, s.behind),
                        Some(s) => format!(
                            "Status: +{} ahead, -{} behind{}",
                            s.ahead,
                            s.behind,
                            if s.dirty { ", uncommitted changes" } else { "" }
                        ),
                    };
                    let attention = st.is_some_and(|s| s.dirty || s.ahead > 0);
                    ui.label(egui::RichText::new(line).color(if attention { th.danger } else { th.dim }));
                    let discardable = orphaned
                        || matches!(
                            card.state,
                            crate::kanban::CardState::Done | crate::kanban::CardState::Blocked
                        );
                    if discardable {
                        #[cfg(test)]
                        {
                            self.offered_discard = true;
                        }
                        if ui
                            .button(egui::RichText::new("Discard worktree").color(th.danger))
                            .on_hover_text("Force-removes the tree and branch, including unmerged work")
                            .clicked()
                        {
                            self.acts.push(BoardAct::DiscardWorktree(card.id.clone()));
                        }
                    }
                }
```

At the top of `show_details` (before `let mut child = …`) reset the probe:

```rust
        #[cfg(test)]
        {
            self.offered_discard = false;
        }
```

- [ ] **Step 5: Run the board tests**

Run: `cargo test --target-dir target/agent board::`
Expected: PASS (existing hit-region tests unchanged: the footer geometry did not move).

- [ ] **Step 6: Commit**

```bash
git add src/board.rs
git commit -m "feat(board): show worktree branch/status on cards and offer Discard on the detail page"
```

---

### Task 7: Docs

**Files:**
- Modify: `docs/kanban-board.md` (new "Worktrees" section in "What it does", a Gotchas bullet, Key files line)
- Modify: `CONTEXT.md` (five glossary entries near **Card**/**Claim**)

- [ ] **Step 1: Update `docs/kanban-board.md`**

Under "What it does", after the **Dispatch from a card** bullet, add:

```markdown
- **Per-card worktrees**: with `dispatch_worktrees` on (the default, Agents
  pane), Start creates `<repo>/.foreman/worktrees/<id>` on branch `card/<id>`
  and spawns the worker there. The card records `worktree` (path, branch,
  and `base` — the branch the main checkout had at dispatch). The prompt's
  Workspace section tells the worker to `git rebase <base>` and fast-forward
  `<base>` from the worktree before `done`. `done`, board Release, and `rm`
  run a non-forcing teardown on a background thread (`worktree remove`,
  `branch -d`, `prune`); a dirty tree or an unmerged branch is kept and the
  card says so. `rm` refuses outright while the tree is dirty or ahead of
  base. `block` and orphaned cards keep the tree so Restart resumes in it.
  Outside a git repository dispatch runs in place silently; on a detached
  HEAD it runs in place with a warning toast. **Discard worktree** on a
  Done/Blocked/orphaned card's detail page is the only forcing path and is
  human-only (no wire verb).
- **Worktree status is derived**: while a board is shown, every worktree
  card is probed every few seconds on a background thread (dirty, ahead,
  behind, missing) and the result is shown on the card face and by
  `foreman kanban list` (`[wt card/<id> +A -B dirty]`; `--json` adds
  `worktree` and `worktree_status`). Nothing about status is written to a
  card file; a hidden board polls nothing.
```

In Gotchas add:

```markdown
- **Each worktree cold-builds.** It has its own `target/`; the first build
  costs minutes (`docs/dev-launcher.md` forbids sharing a target dir). Minutes
  of compile beat corrupted commits.
- **The worktree carries a stale `.foreman/tasks/`.** Close-out reaches the
  project's board through the pipe, so the board is unaffected — but a
  worker that runs `git add -A` commits stale card files and the
  fast-forward carries them into base. The prompt says never to stage
  `.foreman/`; review for it anyway.
- **`kanban list` worktree status comes from the last poll round.** A board
  that has not been shown since the card was dispatched shows the branch
  with no counts.
- **Ignore is per-clone.** Bring-up appends `.foreman/worktrees/` to
  `.git/info/exclude`, never to `.gitignore`.
```

In Key files, extend the `src/kanban.rs` line with `Worktree`/`WorktreeStatus`, `worktree_layout`, `bring_up_worktree`, `teardown_worktree`, `worktree_status_now`, `worktree_summary`; the `src/wm.rs` line with `drain_worktree_msgs`, `start_teardown`, `kanban_rm`; add `src/config.rs` — `Settings::dispatch_worktrees`.

- [ ] **Step 2: Update `CONTEXT.md`**

After the **Claim** entry add, in the file's `**Term**:` / `_Avoid_` style:

```markdown
**Worktree**:
A Card's private checkout under `.foreman/worktrees/<id>`, on branch `card/<id>`,
created at dispatch and recorded on the Card.
_Avoid_: sandbox, checkout (ambiguous with the main checkout).

**Base**:
The branch the main checkout had at dispatch; the Worktree's integration target.
_Avoid_: main, trunk (it may be a feature branch).

**Integrate**:
The Worker's rebase onto Base plus fast-forward of Base, done before `done`.
_Avoid_: merge (implies a merge commit).

**Teardown**:
The non-forcing remove-and-delete run on `done`, Release, and `rm`; keeps a dirty
tree or an unmerged branch.
_Avoid_: cleanup, delete.

**Discard**:
The human-only forcing Teardown offered on a Done, Blocked, or orphaned Card.
_Avoid_: force-delete, purge.
```

- [ ] **Step 3: Run the cite guard and commit**

Run: `pwsh -File .claude/hooks/cite-guard.ps1 -All`
Expected: no output for `docs/kanban-board.md`.

```bash
git add docs/kanban-board.md CONTEXT.md docs/superpowers/plans/2026-09-15-dispatch-worktrees.md
git commit -m "docs(kanban): describe per-card worktrees, teardown, and status"
```

---

## Self-review

- **Spec coverage:** schema (T2), bring-up steps 1–7 (T4, T5), prompt (T3), teardown + outcome table + which transitions (T4, T5), Discard human-only with confirm (T5, T6), status poll + in-memory map (T2, T5), card face + detail + list line + `--json` (T2, T6), `wait` unchanged (nothing touched), setting (T1), every pure seam test listed in the spec (T2–T4), git-backed tests with skip (T4, T5), docs (T7).
- **Type consistency:** `Worktree { path: String, branch, base }`, `WorktreeStatus { dirty, ahead, behind, missing }`, `TeardownOutcome::{Removed, Dirty, Unmerged{ahead}, Failed(String)}`, `BringUp::{Worktree, InPlace, Detached}`, `claim_for_dispatch(.., worktree: Option<Worktree>)`, `kanban_dispatch(req, ctx)`, `resolve_pending(outcome, ctx)` are used with the same shapes in every task.
- **Known judgement calls:** `dispatch_prompt` derives `{root}` from the stored path (three ancestors) instead of storing root on the card, keeping the schema at the spec's three fields; reuse is decided by `git worktree list`, not by the card field, so a claim failure after bring-up cannot strand a tree.
