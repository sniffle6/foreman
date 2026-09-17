# Kanban Cut Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A human (or a release script) Cuts the ungrouped Done cards into a named Version; the Done column can switch between Current and any Version; each shipped card carries the commits that named it in a `Card: <id>` trailer.

**Architecture:** `kanban.rs` keeps the pure domain: the `Shipped` schema, one deep store verb `CardStore::cut` that owns reload, validation, the batch write, and the revert, with its two project-dependent facts (the worktree hold-back probe and the trailer commit lookup) injected as closures so tests never run git; `uncut` beside it; `versions()` and `same_name()` as the single sources of dropdown order and name equality; `parse_trailer_log` as the pure half of the git walk. `control.rs` adds two wire fields and three parser branches. `wm.rs` supplies the closures from the project (`kanban_cut`, the same shape as `kanban_rm`) and drains two new board acts. `board.rs` adds the Done header dropdown and Cut button, the inline name field, the archive banner, the collapsed-rail label, and the detail page's Version and Commits sections.

**Tech Stack:** Rust 2024, egui 0.34.3 (`ComboBox`, `TextEdit`), serde, `std::process::Command` (git, via the existing `git()` helper), `tempfile` in tests.

**Spec:** `docs/superpowers/specs/2026-09-16-kanban-cut-design.md`

## Global Constraints

- Build with `cargo build --target-dir target/agent`; test with `cargo test --target-dir target/agent` (never `--lib`, bin-only crate). Never `Stop-Process foreman` by name.
- `Card` files and `list --json` output must be byte-identical for cards without `shipped` (`skip_serializing_if = "Option::is_none"`); `shipped.commits` is omitted when empty.
- `KanbanRequest.name` and `.all` are skipped on the wire when unset; a v1 request JSON without them still parses.
- Version names compare through `same_name` only (trimmed, case-insensitive). `Current` in any case is never a Version name.
- `CardStore` never runs git. The worktree probe and the trailer walk are injected by `wm.rs`.
- Every git subprocess goes through the existing `git()` helper (`CREATE_NO_WINDOW`).
- Git-backed tests are skipped (early `return`) when `git` is not on PATH, never failed.
- Commit messages: `type(scope): subject` with the `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` trailer. Never stage `.foreman/`.
- Docs update `docs/kanban-board.md`, `CONTEXT.md`, and both `foreman-kanban` skill copies; no new doc file.

---

### Task 1: Schema and pure helpers — `Shipped`, `same_name`, `versions`, list lines, trailer parser

**Files:**
- Modify: `src/kanban.rs` (`struct Card`, `Card::new`, `CardStore::reload`, `CardStore::read_one`, `CardLine::human_line`, the `tests` module)

**Interfaces:**
- Produces: `pub struct Shipped { name: String, at: String, commits: Vec<String> }`; `Card.shipped: Option<Shipped>`; `pub const CURRENT: &str`; `pub fn same_name(a: &str, b: &str) -> bool`; `pub struct Version { name, at, count }`; `pub fn versions(cards: &[Card]) -> Vec<Version>`; `pub fn parse_trailer_log(text: &str) -> HashMap<String, Vec<String>>`. Tasks 2, 3, 5, 6 use all of these by these exact names.

- [ ] **Step 1: Write the failing tests**

In the `tests` module of `src/kanban.rs`, after `v1_card_file_without_worktree_round_trips_unchanged`:

```rust
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
        std::fs::write(
            tmp.path().join("zz0001.json"),
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
    fn human_line_carries_the_version_tail() {
        let mut card = sample_card(None);
        card.state = CardState::Done;
        card.shipped = Some(sample_shipped());
        let line = CardLine {
            card,
            orphaned: false,
            worktree_status: None,
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
        assert_eq!(m.get("x1").unwrap(), &vec!["aaa".to_string(), "ccc".to_string()]);
        assert_eq!(m.get("x2").unwrap(), &vec!["aaa".to_string()]);
        assert_eq!(m.len(), 2);
        assert!(parse_trailer_log("").is_empty());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --target-dir target/agent kanban::tests::shipped_ 2>&1 | tail -20`
Expected: compile errors — `Shipped`, `same_name`, `versions`, `Version`, `CURRENT`, `parse_trailer_log` not found; `Card` has no field `shipped`.

- [ ] **Step 3: Add the schema**

In `src/kanban.rs`, directly above `pub struct Card`:

```rust
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
```

In `pub struct Card`, after the `worktree` field:

```rust
    /// Absent until the card is Cut into a Version (spec: kanban-cut).
    /// `state` stays `done`; this is grouping, not a column.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shipped: Option<Shipped>,
```

In `Card::new`, after `worktree: None,` add `shipped: None,`.

Add to `impl Card` (the block holding `new`):

```rust
    /// Load-time repair: a `shipped` whose name trims to empty (a hand
    /// edit) is no Version at all.
    fn normalize(mut self) -> Self {
        if self
            .shipped
            .as_ref()
            .is_some_and(|s| s.name.trim().is_empty())
        {
            self.shipped = None;
        }
        self
    }
```

In `CardStore::reload`, change `Ok(card) => cards.push(card),` to `Ok(card) => cards.push(card.normalize()),`.

In `CardStore::read_one`, change the last line to:

```rust
        serde_json::from_str::<Card>(&text)
            .map(Card::normalize)
            .map_err(|e| format!("card {id} is corrupt: {e}"))
```

In `CardLine::human_line`, after the `if let Some(wt) = &self.card.worktree { ... }` block:

```rust
        if let Some(s) = &self.card.shipped {
            tail.push(format!("[shipped {}]", s.name));
        }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --target-dir target/agent kanban::tests 2>&1 | tail -5`
Expected: all kanban tests pass, including the seven new ones. `card_parse_tolerates_unknown_fields_and_full_claim` and `v1_card_file_without_worktree_round_trips_unchanged` still pass (the field is skipped when `None`).

- [ ] **Step 5: Commit**

```bash
git add src/kanban.rs
git commit -m "feat(kanban): Shipped schema, version listing, trailer log parser"
```

---

### Task 2: `CardStore::cut` and `CardStore::uncut`

**Files:**
- Modify: `src/kanban.rs` (`impl CardStore`, the `tests` module)

**Interfaces:**
- Consumes: `Shipped`, `CURRENT`, `same_name` (Task 1); `CardStore::write_card`, `reload`, `dir_or_err`, `now_stamp` (existing).
- Produces: `pub struct CutOutcome { name, shipped: Vec<String>, held_back: Vec<(String, String)> }` with `pub fn lines(&self) -> Vec<String>`; `CardStore::cut(&mut self, name: &str, hold: impl Fn(&Card) -> Option<String>, commits: impl FnOnce(&[Card]) -> HashMap<String, Vec<String>>) -> Result<CutOutcome, String>`; `CardStore::uncut(&mut self, name: &str) -> Result<usize, String>`. Task 5 calls both; Task 6 tests call `cut` with no-op closures.

- [ ] **Step 1: Write the failing tests**

In the `tests` module of `src/kanban.rs`, after `parse_trailer_log_buckets_by_id_oldest_first_and_skips_untagged`:

```rust
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
        let before = std::fs::read_to_string(tmp.path().join(format!("{b}.json"))).unwrap();
        let e = s
            .cut("v1", |_| Some("unmerged".into()), no_commits)
            .unwrap_err();
        assert!(e.contains("nothing to cut"), "{e}");
        let after = std::fs::read_to_string(tmp.path().join(format!("{b}.json"))).unwrap();
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
        assert!(s.get(&b).unwrap().shipped.as_ref().unwrap().commits.is_empty());
        let text = std::fs::read_to_string(tmp.path().join(format!("{b}.json"))).unwrap();
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
        std::fs::create_dir(tmp.path().join(format!("{victim}.json.tmp"))).unwrap();
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
        std::fs::write(
            tmp.path().join("zz0002.json"),
            r#"{"v":1,"id":"zz0002","title":"t","state":"done","created":"2026-08-28T13:55:00Z","updated":"2026-08-28T13:55:00Z"}"#,
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("zz0003.json"),
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --target-dir target/agent kanban::tests::cut_ 2>&1 | tail -20`
Expected: compile errors — no method `cut` / `uncut` on `CardStore`, `CutOutcome` not found.

- [ ] **Step 3: Implement the verbs**

In `src/kanban.rs`, directly above `impl CardStore` (the block containing `set_dir`):

```rust
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
```

Inside `impl CardStore`, after `rm`:

```rust
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --target-dir target/agent kanban::tests 2>&1 | tail -5`
Expected: all pass, including the ten new ones.

- [ ] **Step 5: Commit**

```bash
git add src/kanban.rs
git commit -m "feat(kanban): CardStore::cut and uncut with batch write and revert"
```

---

### Task 3: The card trailer in the dispatch prompt, and the git adapters

**Files:**
- Modify: `src/kanban.rs` (`dispatch_prompt`, the six verbatim prompt tests, new git adapters, new git-backed tests)

**Interfaces:**
- Consumes: `parse_trailer_log` (Task 1); `git()`, `git_available` (existing).
- Produces: `pub fn trailer_commits(cwd: &Path, since: &str) -> HashMap<String, Vec<String>>` (fail-open); `pub fn latest_v_tag(cwd: &Path) -> Option<String>` (fail-open). Task 5 calls both.

- [ ] **Step 1: Update the six verbatim prompt tests**

In each of these tests in `src/kanban.rs`, insert one line into the expected string, immediately after the line `# Close-out (required)\n\` and before whatever follows it (`When the work is complete` in the four plain tests, `Integrate first` in the two worktree tests):

```
             End every commit message with the trailer line:    Card: a3f8k2\n\
```

Tests to edit: `dispatch_prompt_path_style_renders_the_spec_template_verbatim`, `dispatch_prompt_path_style_renders_with_no_body`, `dispatch_prompt_envvar_style_renders_the_dev_fleet_template_verbatim`, `dispatch_prompt_envvar_style_renders_with_no_body`, `dispatch_prompt_with_worktree_renders_workspace_and_integration_lines`, `dispatch_prompt_with_worktree_envvar_style_keeps_git_lines_style_independent`.

The path-style test's expected string becomes exactly:

```rust
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
```

- [ ] **Step 2: Write the failing git-backed tests**

After `teardown_keeps_an_unmerged_branch_and_reports_the_count` in the `tests` module:

```rust
    #[test]
    fn trailer_commits_reads_real_trailers_oldest_first() {
        let Some(repo) = git_repo() else { return };
        git_in(repo.path(), &["commit", "-q", "--allow-empty", "-m", "one\n\nCard: a1b2c3"]);
        let one = git_in(repo.path(), &["rev-parse", "--short", "HEAD"]);
        git_in(repo.path(), &["commit", "-q", "--allow-empty", "-m", "two, no trailer"]);
        git_in(
            repo.path(),
            &["commit", "-q", "--allow-empty", "-m", "three\n\nCard: a1b2c3\nCard: d4e5f6"],
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
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test --target-dir target/agent kanban::tests::dispatch_prompt 2>&1 | tail -20` and `cargo test --target-dir target/agent kanban::tests::trailer_commits 2>&1 | tail -20`
Expected: the six prompt tests fail on the missing line; the git tests fail to compile (`trailer_commits`, `latest_v_tag` not found).

- [ ] **Step 4: Implement**

In `dispatch_prompt`, directly after `out.push_str("# Close-out (required)\n");`:

```rust
    // The card trailer (spec: kanban-cut §The card trailer): the only
    // per-commit signal that survives rebase and squash, read back at Cut.
    out.push_str(&format!(
        "End every commit message with the trailer line:    Card: {id}\n",
        id = card.id
    ));
```

After `worktree_status_now` (a free function), add:

```rust
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
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --target-dir target/agent kanban::tests 2>&1 | tail -5`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add src/kanban.rs
git commit -m "feat(kanban): card trailer in the dispatch prompt; trailer walk and tag prefill adapters"
```

---

### Task 4: Wire shape, parsers, help

**Files:**
- Modify: `src/control.rs` (`KanbanRequest`, `parse_kanban_args`, `parse_kanban_list`, new `parse_kanban_named`, `HELP_KANBAN`, tests)

**Interfaces:**
- Produces: `KanbanRequest.name: Option<String>`, `KanbanRequest.all: bool`; `action` values `"cut"` and `"uncut"`. Task 5 reads `req.name` and `req.all`.

- [ ] **Step 1: Write the failing tests**

Extend `kanban_request_wire_roundtrips_and_omits_unset_fields`: change the `assert!` line to

```rust
        assert!(
            !j.contains("\"id\"")
                && !j.contains("\"title\"")
                && !j.contains("\"json\"")
                && !j.contains("\"name\"")
                && !j.contains("\"all\"")
        );
        // A v1 request (no `name`, no `all`) still parses.
        let v1: KanbanRequest =
            serde_json::from_str(r#"{"cmd":"kanban","action":"list"}"#).unwrap();
        assert_eq!(v1, req);
```

After `parse_kanban_args_list_happy_path_and_bad_state`, add:

```rust
    #[test]
    fn parse_kanban_args_cut_and_uncut_take_one_name() {
        for verb in ["cut", "uncut"] {
            let req = match parse_kanban_args(&s(&[verb, "v0.5.0"]), Some("p1".into()), None)
                .unwrap()
            {
                KanbanAction::Request(r) => r,
                _ => panic!("expected a request"),
            };
            assert_eq!(req.action, verb);
            assert_eq!(req.name.as_deref(), Some("v0.5.0"));
            assert_eq!(req.project.as_deref(), Some("p1"));
            let e = parse_kanban_args(&s(&[verb]), None, None).unwrap_err();
            assert!(e.contains("<name>"), "{e}");
            let e = parse_kanban_args(&s(&[verb, "a", "b"]), None, None).unwrap_err();
            assert!(e.contains("unexpected"), "{e}");
        }
    }

    #[test]
    fn parse_kanban_args_list_shipped_and_all_rules() {
        let req = match parse_kanban_args(&s(&["list", "--shipped", "v1", "--json"]), None, None)
            .unwrap()
        {
            KanbanAction::Request(r) => r,
            _ => panic!("expected a request"),
        };
        assert_eq!(req.name.as_deref(), Some("v1"));
        assert!(req.json && !req.all);
        // --state done with --shipped is redundant but legal
        assert!(parse_kanban_args(&s(&["list", "--shipped", "v1", "--state", "done"]), None, None).is_ok());
        let e = parse_kanban_args(&s(&["list", "--shipped", "v1", "--state", "backlog"]), None, None)
            .unwrap_err();
        assert!(e.contains("--shipped"), "{e}");
        let req = match parse_kanban_args(&s(&["list", "--all"]), None, None).unwrap() {
            KanbanAction::Request(r) => r,
            _ => panic!("expected a request"),
        };
        assert!(req.all && req.name.is_none());
        let e = parse_kanban_args(&s(&["list", "--all", "--shipped", "v1"]), None, None).unwrap_err();
        assert!(e.contains("--all"), "{e}");
        let e = parse_kanban_args(&s(&["list", "--all", "--state", "done"]), None, None).unwrap_err();
        assert!(e.contains("--all"), "{e}");
        let e = parse_kanban_args(&s(&["list", "--shipped"]), None, None).unwrap_err();
        assert!(e.contains("needs a value"), "{e}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --target-dir target/agent control::tests::parse_kanban_args 2>&1 | tail -20`
Expected: compile error — no field `name` / `all` on `KanbanRequest`.

- [ ] **Step 3: Implement the wire shape and parsers**

In `KanbanRequest`, update the `action` comment to `// "add" | "list" | "start" | "done" | "block" | "rm" | "cut" | "uncut"` and add after the `json` field:

```rust
    /// Version name for `cut`, `uncut`, and `list --shipped` (spec:
    /// kanban-cut §List and wire). Skipped when unset so v1 requests stay
    /// byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `list --all`: include shipped cards (bare `list` is the live board).
    #[serde(default, skip_serializing_if = "is_false")]
    pub all: bool,
```

In `parse_kanban_args`, add two arms before `other =>`:

```rust
        "cut" => parse_kanban_named(rest, default_project, "cut"),
        "uncut" => parse_kanban_named(rest, default_project, "uncut"),
```

After `parse_kanban_simple`, add:

```rust
/// `cut <name> [--project P]` / `uncut <name> [--project P]` — one
/// positional, the Version name. The CLI never defaults it from git (spec:
/// kanban-cut §Decisions); only the board prefills.
fn parse_kanban_named(
    args: &[String],
    default_project: Option<String>,
    action: &str,
) -> Result<KanbanAction, String> {
    let (name, project) = parse_kanban_id_and_project(args, default_project)
        .map_err(|e| e.replace("<id>", "<name>"))?;
    Ok(KanbanAction::Request(KanbanRequest {
        cmd: "kanban".into(),
        action: action.into(),
        project,
        name: Some(name),
        ..Default::default()
    }))
}
```

In `parse_kanban_list`: update the doc comment to `` /// `list [--state backlog|in_progress|blocked|done] [--shipped NAME] [--all] [--json] [--project P]`. ``; add `let mut shipped: Option<String> = None;` and `let mut all = false;` beside `json`; add two match arms before `other if other.starts_with("--")`:

```rust
            "--shipped" => {
                shipped = Some(args.get(i + 1).ok_or("--shipped needs a value")?.clone());
                i += 2;
            }
            "--all" => {
                all = true;
                i += 1;
            }
```

After the `while` loop, before `Ok(...)`:

```rust
    if all && (shipped.is_some() || state.is_some()) {
        return Err("--all lists every card; it cannot combine with --shipped or --state".into());
    }
    if shipped.is_some() && state.as_deref().is_some_and(|s| s != "done") {
        return Err("--shipped lists Done cards; --state must be done or omitted".into());
    }
```

And in the returned `KanbanRequest`, add `name: shipped,` and `all,`.

- [ ] **Step 4: Update `HELP_KANBAN`**

Replace the `list` usage line with:

```
foreman kanban list [--state backlog|in_progress|blocked|done] [--shipped NAME] [--all] [--json] [--project P]
```

After the `rm` usage line add:

```
foreman kanban cut <name> [--project P]
foreman kanban uncut <name> [--project P]
```

In the `list` description, after the sentence ending `worktree appends [wt card/<id> +ahead -behind dirty|missing]).` insert:

```
          Bare list is the live board: shipped cards (Cut into a Version)
          are hidden, and a shipped card's line ends [shipped NAME].
          --shipped NAME lists that Version only (case-insensitive; unknown
          name = empty, exit 0). --all includes every shipped card.
```

After the `rm` description add:

```
  cut     stamp every ungrouped Done card as shipped in Version <name>: the
          ship ritual, run after tagging. Refuses a blank, duplicate, or
          \"Current\" name, an empty Done, or a Done with no merged card. A
          Done card whose worktree is still ahead of base stays in Current
          and is named in the reply. Each shipped card records the commits
          whose \"Card: <id>\" trailer named it.
  uncut   clear <name> from every card in that Version; they return to
          Current Done. Errors on an unknown name.
```

Change `Exit codes for add/list/start/done/block/rm:` to `Exit codes for add/list/start/done/block/rm/cut/uncut:`.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --target-dir target/agent control::tests 2>&1 | tail -5`
Expected: all pass. Also `cargo run --target-dir target/agent -- kanban --help | head -12` shows the new lines.

- [ ] **Step 6: Commit**

```bash
git add src/control.rs
git commit -m "feat(control): kanban cut/uncut verbs, list --shipped and --all, help"
```

---

### Task 5: Window-manager seams — `kanban_cut`, wire arms, list filters, board acts

**Files:**
- Modify: `src/wm.rs` (`impl WindowManager`: new `kanban_cut`, `kanban_dispatch`, `drain_board_acts`; tests)
- Modify: `src/board.rs` (`BoardAct` — the three new variants only; their producers land in Task 6)

**Interfaces:**
- Consumes: `CardStore::cut` / `uncut`, `CutOutcome::lines`, `trailer_commits`, `latest_v_tag`, `versions`, `same_name`, `worktree_status_now` (Tasks 1 to 3); `KanbanRequest.name` / `.all` (Task 4).
- Produces: `BoardAct::Cut(String)`, `BoardAct::Uncut(String)`, `BoardAct::CutPrefill`; `WindowManager::kanban_cut(&mut self, name: &str) -> Result<CutOutcome, String>`; calls `BoardView::prefill_cut(&mut self, name: &str)` (Task 6 defines it; add a stub here so this task compiles).

- [ ] **Step 1: Add the act variants and the stub**

In `src/board.rs`, `pub enum BoardAct`, after `DiscardWorktree(String)`:

```rust
    /// Cut the ungrouped Done cards into a Version (spec: kanban-cut §Cut).
    /// The manager owns the worktree probe and the trailer walk.
    Cut(String),
    /// Return a Version's cards to Current Done (spec §Uncut).
    Uncut(String),
    /// The Cut field just opened empty; the manager answers with the latest
    /// unused `v*` tag via `BoardView::prefill_cut`, or with nothing.
    CutPrefill,
```

In `impl BoardView`, after `new`, a stub Task 6 fills in:

```rust
    /// Manager's answer to `BoardAct::CutPrefill`. Fills the open Cut field
    /// only while it is still empty and untouched.
    pub fn prefill_cut(&mut self, _name: &str) {}
```

- [ ] **Step 2: Write the failing wire-path test**

In the `tests` module of `src/wm.rs`, after `kanban_closeout_verbs_enforce_the_table_over_the_wire_path`:

```rust
    #[test]
    fn kanban_cut_and_uncut_over_the_wire_path_and_list_hides_shipped() {
        let tmp = tempfile::tempdir().unwrap();
        let mut m = kanban_desktop(tmp.path().to_path_buf());
        let pid = m.resolve_project(None).unwrap();
        // Two Done cards and one Backlog card, made through the store (no
        // PTY needed): add, dispatch-claim, done.
        let (a, b, backlog) = {
            let child = m.project_child_mut(pid).unwrap();
            child.kanban.borrow_mut().set_dir(Some(tmp.path()));
            let mut s = child.kanban.borrow_mut();
            let mut done = |title: &str| {
                let id = s.add(title, None).unwrap();
                s.claim_for_dispatch(
                    &id,
                    "t1",
                    "claude",
                    crate::kanban::run_nonce(),
                    crate::kanban::TermState::Missing,
                    None,
                )
                .unwrap();
                s.done(&id).unwrap();
                id
            };
            let a = done("a");
            let b = done("b");
            let backlog = s.add("later", None).unwrap();
            (a, b, backlog)
        };

        let mut cut = kanban_req("cut");
        assert!(m.kanban_dispatch(&cut).unwrap_err().contains("name"));
        cut.name = Some("v1".into());
        let reply = m.kanban_dispatch(&cut).unwrap();
        assert!(reply.ok);
        assert_eq!(reply.history.unwrap(), vec!["cut v1: 2 cards".to_string()]);
        assert!(m.kanban_dispatch(&cut).unwrap_err().contains("already exists"));

        // bare list: the live board — backlog only
        let list = kanban_req("list");
        let lines = m.kanban_dispatch(&list).unwrap().history.unwrap();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with(&backlog), "{lines:?}");
        // --state done: Current Done — empty now
        let mut done_only = kanban_req("list");
        done_only.state = Some("done".into());
        assert!(m.kanban_dispatch(&done_only).unwrap().history.unwrap().is_empty());
        // --shipped V1: that Version, case-insensitive; --json carries shipped
        let mut shipped = kanban_req("list");
        shipped.name = Some("V1".into());
        shipped.json = true;
        let lines = m.kanban_dispatch(&shipped).unwrap().history.unwrap();
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|l| l.contains(r#""shipped":{"name":"v1""#)), "{lines:?}");
        // --shipped unknown: empty, ok
        shipped.name = Some("nope".into());
        let reply = m.kanban_dispatch(&shipped).unwrap();
        assert!(reply.ok && reply.history.unwrap().is_empty());
        // --all: everything, human lines tagged
        let mut all = kanban_req("list");
        all.all = true;
        let lines = m.kanban_dispatch(&all).unwrap().history.unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines.iter().filter(|l| l.contains("[shipped v1]")).count(), 2);

        let mut uncut = kanban_req("uncut");
        uncut.name = Some("v1".into());
        assert_eq!(
            m.kanban_dispatch(&uncut).unwrap().history.unwrap(),
            vec!["uncut v1: 2 cards".to_string()]
        );
        assert!(m.kanban_dispatch(&uncut).unwrap_err().contains("no version"));
        let lines = m.kanban_dispatch(&done_only).unwrap().history.unwrap();
        assert_eq!(lines.len(), 2, "{lines:?}");
        let _ = (a, b);
    }
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test --target-dir target/agent wm::tests::kanban_cut_and_uncut 2>&1 | tail -20`
Expected: FAIL — `kanban_dispatch` returns `unknown kanban action: cut`.

- [ ] **Step 4: Implement `kanban_cut` and the wire arms**

In `src/wm.rs`, `impl WindowManager`, directly after `kanban_rm`:

```rust
    /// Cut (spec: kanban-cut §Cut). The store owns validation, the batch
    /// write, and the revert; this seam supplies the two facts that need
    /// the project: the worktree hold-back probe (the `rm` pre-check
    /// verdict — fail closed, an unprobeable tree stays in Current) and the
    /// trailer walk, bounded by the oldest candidate's `created`.
    fn kanban_cut(&mut self, name: &str) -> Result<crate::kanban::CutOutcome, String> {
        let cwd = self
            .cwd
            .clone()
            .ok_or("project has no working directory")?;
        let hold_cwd = cwd.clone();
        let hold = move |c: &crate::kanban::Card| -> Option<String> {
            let wt = c.worktree.as_ref()?;
            match crate::kanban::worktree_status_now(&hold_cwd, wt) {
                Err(e) => Some(format!("cannot verify worktree: {e}")),
                Ok(st) if st.dirty || st.ahead > 0 => Some(format!("unmerged {}", wt.branch)),
                Ok(_) => None,
            }
        };
        let commits = move |cards: &[crate::kanban::Card]| {
            let since = cards
                .iter()
                .map(|c| c.created.as_str())
                .min()
                .unwrap_or("1970-01-01T00:00:00Z");
            crate::kanban::trailer_commits(&cwd, since)
        };
        self.kanban.borrow_mut().cut(name, hold, commits)
    }

    /// The Cut field's prefill (spec §Cut, Board): the newest `v*` tag,
    /// only when that string is not already a Version. Fail-open.
    fn cut_prefill(&self) -> Option<String> {
        let tag = crate::kanban::latest_v_tag(self.cwd.as_deref()?)?;
        let taken = crate::kanban::versions(self.kanban.borrow().cards())
            .iter()
            .any(|v| crate::kanban::same_name(&v.name, &tag));
        (!taken).then_some(tag)
    }
```

In `kanban_dispatch`, add two arms before `other =>`:

```rust
            "cut" => {
                let name = req.name.as_deref().ok_or("cut requires a version name")?;
                let out = child.kanban_cut(name)?;
                Ok(OpenReply {
                    ok: true,
                    history: Some(out.lines()),
                    ..Default::default()
                })
            }
            "uncut" => {
                let name = req.name.as_deref().ok_or("uncut requires a version name")?;
                let n = child.kanban.borrow_mut().uncut(name)?;
                Ok(OpenReply {
                    ok: true,
                    history: Some(vec![format!("uncut {name}: {n} cards")]),
                    ..Default::default()
                })
            }
```

In the `"list"` arm, replace the `.filter(|c| filter.map(|f| c.state == f).unwrap_or(true))` line with:

```rust
                    .filter(|c| match req.name.as_deref() {
                        // --shipped NAME: that Version only, any state filter
                        // was validated client-side to be `done` or absent.
                        Some(v) => c
                            .shipped
                            .as_ref()
                            .is_some_and(|s| crate::kanban::same_name(&s.name, v)),
                        // bare / --state: the live board; --all: everything.
                        None => {
                            (req.all || c.shipped.is_none())
                                && filter.map(|f| c.state == f).unwrap_or(true)
                        }
                    })
```

- [ ] **Step 5: Drain the board acts**

In `drain_board_acts`, add three arms before `crate::board::BoardAct::JumpTo(tag) =>`:

```rust
                crate::board::BoardAct::Cut(name) => match self.kanban_cut(&name) {
                    Ok(out) => crate::notify::queue(
                        ctx,
                        if out.held_back.is_empty() {
                            crate::notify::Level::Success
                        } else {
                            crate::notify::Level::Warning
                        },
                        out.lines().join("; "),
                    ),
                    Err(e) => crate::notify::queue(
                        ctx,
                        crate::notify::Level::Error,
                        format!("board: cut failed: {e}"),
                    ),
                },
                crate::board::BoardAct::Uncut(name) => {
                    let res = self.kanban.borrow_mut().uncut(&name);
                    if let Err(e) = res {
                        crate::notify::queue(
                            ctx,
                            crate::notify::Level::Error,
                            format!("board: uncut failed: {e}"),
                        );
                    }
                }
                crate::board::BoardAct::CutPrefill => {
                    if let Some(tag) = self.cut_prefill() {
                        for w in &mut self.windows {
                            for t in &mut w.tabs {
                                if let Content::Board(v) = &mut t.content {
                                    v.prefill_cut(&tag);
                                }
                            }
                        }
                    }
                }
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --target-dir target/agent wm::tests::kanban 2>&1 | tail -5`
Expected: all kanban wm tests pass, including the new one. Run `cargo build --target-dir target/agent 2>&1 | grep -E "^(warning|error)" | sort | uniq -c` and confirm no new warnings beyond the baseline (`foreman-build-and-env` lists it).

- [ ] **Step 7: Commit**

```bash
git add src/wm.rs src/board.rs
git commit -m "feat(wm): kanban cut/uncut over the wire and from the board; list hides shipped cards"
```

---

### Task 6: Board surfaces — dropdown, Cut button and field, banner, rail, details

**Files:**
- Modify: `src/board.rs` (`BoardView` fields and `new`, `prefill_cut`, `show`, `show_column`, `show_details`, tests)

**Interfaces:**
- Consumes: `versions`, `same_name`, `CURRENT`, `Card.shipped` (Task 1); `BoardAct::Cut` / `Uncut` / `CutPrefill` (Task 5).
- Produces: `BoardView.version: Option<String>` (`pub(crate)` for tests), `BoardView::prefill_cut` (real body), test probes `offered_cut`, `drew_banner`, `done_listed`.

- [ ] **Step 1: Write the failing tests**

In the `tests` module of `src/board.rs`, after `detail_page_offers_discard_only_for_done_blocked_or_orphaned_worktree_cards`:

```rust
    /// Add a card, claim it as if dispatched, close it out.
    fn add_done(store: &Rc<RefCell<crate::kanban::CardStore>>, title: &str) -> String {
        let mut s = store.borrow_mut();
        let id = s.add(title, None).unwrap();
        s.claim_for_dispatch(
            &id,
            "t1",
            "claude",
            crate::kanban::run_nonce(),
            crate::kanban::TermState::Missing,
            None,
        )
        .unwrap();
        s.done(&id).unwrap();
        id
    }

    fn cut(store: &Rc<RefCell<crate::kanban::CardStore>>, name: &str) {
        store
            .borrow_mut()
            .cut(name, |_| None, |_| Default::default())
            .unwrap();
    }

    /// Centre of the Done header's Cut button at scale 1 (rightmost control).
    fn cut_button_pos(rect: egui::Rect) -> egui::Pos2 {
        egui::pos2(rect.max.x - PAD - CUT_W / 2.0, rect.min.y + HEADER_H / 2.0)
    }

    /// Centre of the Done header's version dropdown at scale 1.
    fn dropdown_pos(rect: egui::Rect) -> egui::Pos2 {
        egui::pos2(
            rect.max.x - PAD - CUT_W - BTN_GAP - DD_W / 2.0,
            rect.min.y + HEADER_H / 2.0,
        )
    }

    #[test]
    fn done_header_offers_cut_only_in_current_with_ungrouped_done() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_at(tmp.path());
        let mut board = BoardView::new(Rc::clone(&store));
        let ctx = egui::Context::default();
        let base = egui::Id::new("cut-gating");
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 400.0));
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert!(!board.offered_cut, "empty Done: Cut disabled");
        assert!(!board.drew_banner);

        let a = add_done(&store, "a");
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert!(board.offered_cut);
        assert_eq!(board.done_listed, vec![a.clone()]);

        cut(&store, "v1");
        let b = add_done(&store, "b");
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert_eq!(board.done_listed, vec![b.clone()], "Current hides shipped cards");

        board.version = Some("v1".into());
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert!(!board.offered_cut, "no Cut inside a Version");
        assert!(board.drew_banner);
        assert_eq!(board.done_listed, vec![a.clone()]);

        // Collapsing Done keeps the selection.
        board.collapsed[3] = true;
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert_eq!(board.version.as_deref(), Some("v1"));
        board.collapsed[3] = false;

        // The Version vanishing (uncut) snaps the dropdown back to Current.
        store.borrow_mut().uncut("v1").unwrap();
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert!(board.version.is_none());
        assert!(!board.drew_banner);
        let mut listed = board.done_listed.clone();
        listed.sort();
        let mut want = vec![a, b];
        want.sort();
        assert_eq!(listed, want);
    }

    #[test]
    fn cut_button_opens_the_field_prefill_lands_and_enter_records_the_act() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_at(tmp.path());
        add_done(&store, "a");
        let mut board = BoardView::new(Rc::clone(&store));
        let ctx = egui::Context::default();
        let base = egui::Id::new("cut-field");
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 400.0));
        click_at(&ctx, &mut board, rect, base, cut_button_pos(rect));
        assert_eq!(board.cut_field.as_deref(), Some(""));
        assert!(matches!(board.acts.pop(), Some(BoardAct::CutPrefill)));
        assert_eq!(board.collapsed, [false; 4], "the button must not collapse Done");
        board.prefill_cut("v0.5.0");
        assert_eq!(board.cut_field.as_deref(), Some("v0.5.0"));
        // A late prefill never overwrites text.
        board.prefill_cut("v9.9.9");
        assert_eq!(board.cut_field.as_deref(), Some("v0.5.0"));
        let enter = egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        };
        run_frame(&ctx, &mut board, rect, base, vec![enter]);
        assert!(
            matches!(board.acts.as_slice(), [BoardAct::Cut(n)] if n == "v0.5.0"),
            "{:?}",
            board.acts.len()
        );
        assert!(board.cut_field.is_none(), "Enter closes the field");
    }

    #[test]
    fn dropdown_click_does_not_collapse_done() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_at(tmp.path());
        add_done(&store, "a");
        cut(&store, "v1");
        let mut board = BoardView::new(Rc::clone(&store));
        let ctx = egui::Context::default();
        let base = egui::Id::new("dd-click");
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 400.0));
        click_at(&ctx, &mut board, rect, base, dropdown_pos(rect));
        assert_eq!(board.collapsed, [false; 4]);
        assert!(board.acts.is_empty());
    }

    #[test]
    fn detail_page_shows_version_and_commits_for_a_shipped_card() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_at(tmp.path());
        let id = add_done(&store, "a");
        let id2 = id.clone();
        store
            .borrow_mut()
            .cut("v1", |_| None, move |_| {
                let mut m: std::collections::HashMap<String, Vec<String>> = Default::default();
                m.insert(id2.clone(), vec!["abc1234".into()]);
                m
            })
            .unwrap();
        let mut board = BoardView::new(Rc::clone(&store));
        board.selected = Some(id);
        let ctx = egui::Context::default();
        let base = egui::Id::new("detail-shipped");
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert_eq!(board.detail_commits, vec!["abc1234".to_string()]);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --target-dir target/agent board::tests::done_header 2>&1 | tail -20`
Expected: compile errors — no field `offered_cut` / `drew_banner` / `done_listed` / `version` / `cut_field` / `detail_commits`, no const `CUT_W` / `DD_W`.

- [ ] **Step 3: State, constants, `prefill_cut`**

In `src/board.rs`, after `const WT_CHIP_W: f32 = 36.0;`:

```rust
/// Done header controls (spec: kanban-cut §Board UI): the version dropdown
/// and, in Current, the Cut button — right-anchored, own hit regions.
const DD_W: f32 = 120.0;
const CUT_W: f32 = 40.0;
```

In `pub struct BoardView`, after `scale: f32,`:

```rust
    /// Which Done view is showing: `None` = Current, `Some(name)` = that
    /// Version (spec §Board UI). View state, never persisted — same rule as
    /// `collapsed`.
    pub(crate) version: Option<String>,
    /// The open Cut name field's buffer; `None` = closed.
    pub(crate) cut_field: Option<String>,
    /// True once the human typed into the field, so a late prefill never
    /// overwrites text.
    cut_touched: bool,
    /// Focus the Cut field on the first frame after it opens.
    cut_focus_pending: bool,
    /// Test probes: what the last frame drew for Done.
    #[cfg(test)]
    pub(crate) offered_cut: bool,
    #[cfg(test)]
    pub(crate) drew_banner: bool,
    #[cfg(test)]
    pub(crate) done_listed: Vec<String>,
    #[cfg(test)]
    pub(crate) detail_commits: Vec<String>,
```

In `new`, after `acts: Vec::new(),`:

```rust
            version: None,
            cut_field: None,
            cut_touched: false,
            cut_focus_pending: false,
            #[cfg(test)]
            offered_cut: false,
            #[cfg(test)]
            drew_banner: false,
            #[cfg(test)]
            done_listed: Vec::new(),
            #[cfg(test)]
            detail_commits: Vec::new(),
```

Replace the Task 5 stub `prefill_cut` with:

```rust
    /// Manager's answer to `BoardAct::CutPrefill`. Fills the open Cut field
    /// only while it is still empty and untouched.
    pub fn prefill_cut(&mut self, name: &str) {
        if self.cut_touched {
            return;
        }
        if let Some(buf) = &mut self.cut_field
            && buf.is_empty()
        {
            *buf = name.to_string();
        }
    }
```

- [ ] **Step 4: Snap-back in `show`**

In `show`, directly after the `let (cards, orphans) = { ... };` block:

```rust
        // A selected Version that no longer exists (Uncut, `rm` of its last
        // card, a pull) snaps back to Current (spec §Uncut).
        if let Some(v) = self.version.clone()
            && !crate::kanban::versions(&cards)
                .iter()
                .any(|x| crate::kanban::same_name(&x.name, &v))
        {
            self.version = None;
        }
        #[cfg(test)]
        {
            self.offered_cut = false;
            self.drew_banner = false;
            self.done_listed.clear();
        }
```

- [ ] **Step 5: The Done column in `show_column`**

Replace the first statement of `show_column` (the `let matching: Vec<&Card> = ...` filter) with:

```rust
        let is_done = state == crate::kanban::CardState::Done;
        let matching: Vec<&crate::kanban::Card> = cards
            .iter()
            .filter(|c| {
                c.state == state
                    && (!is_done
                        || match &self.version {
                            None => c.shipped.is_none(),
                            Some(v) => c
                                .shipped
                                .as_ref()
                                .is_some_and(|s| crate::kanban::same_name(&s.name, v)),
                        })
            })
            .collect();
        #[cfg(test)]
        if is_done {
            self.done_listed = matching.iter().map(|c| c.id.clone()).collect();
        }
```

In the collapsed branch, replace the `let text = format!(...)` with:

```rust
            let text = match (&self.version, is_done) {
                (Some(v), true) => format!("Done\n{v}\n{}", matching.len()),
                _ => format!(
                    "{}\n{}",
                    column_title(state).replace(' ', "\n"),
                    matching.len()
                ),
            };
```

In the expanded branch, replace everything from `let header_rect = ...` through the collapse `if ui.interact(header_rect, ...).clicked() { ... }` block with:

```rust
        let header_rect = egui::Rect::from_min_size(
            col_rect.min,
            egui::vec2(col_rect.width(), HEADER_H * self.scale),
        );
        // Done carries right-anchored controls; the collapse click target is
        // only the title to their left (spec §Board UI).
        let mut title_rect = header_rect;
        let mut dd_rect = None;
        let mut cut_rect = None;
        if is_done {
            let inset = 4.0 * self.scale;
            let cut = egui::Rect::from_min_size(
                egui::pos2(
                    header_rect.max.x - PAD * self.scale - CUT_W * self.scale,
                    header_rect.min.y + inset,
                ),
                egui::vec2(CUT_W * self.scale, header_rect.height() - inset * 2.0),
            );
            let dd = egui::Rect::from_min_size(
                egui::pos2(cut.min.x - BTN_GAP * self.scale - DD_W * self.scale, cut.min.y),
                egui::vec2(DD_W * self.scale, cut.height()),
            );
            title_rect.max.x = dd.min.x - BTN_GAP * self.scale;
            dd_rect = Some(dd);
            cut_rect = Some(cut);
        }
        disclosure(
            &p.with_clip_rect(title_rect),
            egui::pos2(
                title_rect.min.x + 10.0 * self.scale,
                title_rect.center().y,
            ),
            self.scale,
            false,
            th.dim,
        );
        p.with_clip_rect(title_rect).text(
            egui::pos2(
                title_rect.min.x + 20.0 * self.scale,
                title_rect.center().y,
            ),
            egui::Align2::LEFT_CENTER,
            format!("{}  ({})", column_title(state), matching.len()),
            egui::FontId::proportional(11.5 * self.scale),
            th.dim,
        );
        if ui
            .interact(
                title_rect,
                base.with((col_idx, "collapse")),
                egui::Sense::click(),
            )
            .on_hover_text("Collapse column")
            .clicked()
        {
            self.collapsed[col_idx] = true;
            self.picker = None;
        }

        if let Some(dd) = dd_rect {
            // Version dropdown: Current pinned, then Versions newest first.
            let versions = crate::kanban::versions(cards);
            let selected_text = self
                .version
                .clone()
                .unwrap_or_else(|| crate::kanban::CURRENT.to_string());
            let mut pick: Option<Option<String>> = None;
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt(base.with((col_idx, "version-ui")))
                    .max_rect(dd),
            );
            egui::ComboBox::from_id_salt(base.with((col_idx, "version")))
                .width(dd.width())
                .selected_text(selected_text)
                .show_ui(&mut child, |ui| {
                    if ui
                        .selectable_label(self.version.is_none(), crate::kanban::CURRENT)
                        .clicked()
                    {
                        pick = Some(None);
                    }
                    for v in &versions {
                        let on = self
                            .version
                            .as_deref()
                            .is_some_and(|s| crate::kanban::same_name(s, &v.name));
                        if ui
                            .selectable_label(on, format!("{} ({})", v.name, v.count))
                            .clicked()
                        {
                            pick = Some(Some(v.name.clone()));
                        }
                    }
                });
            if let Some(p) = pick {
                self.version = p;
                self.cut_field = None;
                self.picker = None;
            }
        }
        if let Some(cut) = cut_rect {
            if self.version.is_none() {
                let enabled = !matching.is_empty();
                #[cfg(test)]
                {
                    self.offered_cut = enabled;
                }
                let sense = if enabled {
                    egui::Sense::click()
                } else {
                    egui::Sense::hover()
                };
                let r = ui.interact(cut, base.with((col_idx, "cut")), sense);
                p.rect_filled(
                    cut,
                    3.0,
                    if enabled && r.hovered() { th.sel_bg } else { th.bg },
                );
                p.rect_stroke(
                    cut,
                    3.0,
                    egui::Stroke::new(1.0, th.border),
                    egui::StrokeKind::Inside,
                );
                p.text(
                    cut.center(),
                    egui::Align2::CENTER_CENTER,
                    "Cut",
                    egui::FontId::proportional(10.5 * self.scale),
                    if enabled { th.text } else { th.dim },
                );
                let r = r.on_hover_text(if enabled {
                    "Cut Done into a named Version"
                } else {
                    "Nothing in Done to cut"
                });
                if enabled && r.clicked() && self.cut_field.is_none() {
                    self.cut_field = Some(String::new());
                    self.cut_touched = false;
                    self.cut_focus_pending = true;
                    self.acts.push(BoardAct::CutPrefill);
                }
            }
        }
```

Then, after the Backlog quick-add block (`if state == CardState::Backlog { ... }`) and before `let body_rect = ...`, add the Done rows:

```rust
        if is_done && self.version.is_none() && self.cut_field.is_some() {
            // The Cut name field: same inline shape as quick-add, no modal.
            let field_rect = egui::Rect::from_min_size(
                egui::pos2(col_rect.min.x + PAD * self.scale, body_top),
                egui::vec2(
                    (col_rect.width() - PAD * self.scale * 2.0).max(0.0),
                    QUICK_ADD_H * self.scale - 4.0 * self.scale,
                ),
            );
            ui.visuals_mut().selection.bg_fill = th.selection_text_bg;
            let buf = self.cut_field.as_mut().expect("checked above");
            let te = ui.put(
                field_rect,
                egui::TextEdit::singleline(buf)
                    .id(base.with((col_idx, "cut-name")))
                    .font(egui::FontId::proportional(11.5 * self.scale))
                    .text_color(th.text)
                    .hint_text("version name…  Enter cuts, Esc cancels")
                    .vertical_align(egui::Align::Center)
                    .frame(egui::Frame::NONE)
                    .margin(egui::Margin::symmetric(4, 0))
                    .desired_width(field_rect.width()),
            );
            if std::mem::take(&mut self.cut_focus_pending) {
                te.request_focus();
            }
            if te.changed() {
                self.cut_touched = true;
            }
            let (enter, esc) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::Enter),
                    i.key_pressed(egui::Key::Escape),
                )
            });
            if te.lost_focus() && esc {
                self.cut_field = None;
            } else if te.lost_focus() && enter {
                let name = self.cut_field.take().unwrap_or_default();
                let name = name.trim().to_string();
                if name.is_empty() {
                    // Empty is a no-op, not a toast: nothing was asked for.
                } else {
                    self.acts.push(BoardAct::Cut(name));
                }
            }
            body_top += QUICK_ADD_H * self.scale;
        }
        if is_done && let Some(v) = self.version.clone() {
            // Archive banner: the signal that Done is not Current.
            #[cfg(test)]
            {
                self.drew_banner = true;
            }
            let banner = egui::Rect::from_min_size(
                egui::pos2(col_rect.min.x, body_top),
                egui::vec2(col_rect.width(), QUICK_ADD_H * self.scale),
            );
            p.rect_filled(banner, 0.0, th.title_bg);
            p.with_clip_rect(banner).text(
                egui::pos2(banner.min.x + PAD * self.scale, banner.center().y),
                egui::Align2::LEFT_CENTER,
                format!("Archived · {v}"),
                egui::FontId::proportional(11.0 * self.scale),
                th.dim,
            );
            let uncut = egui::Rect::from_min_size(
                egui::pos2(
                    banner.max.x - PAD * self.scale - BTN_W * self.scale,
                    banner.min.y + 2.0 * self.scale,
                ),
                egui::vec2(BTN_W * self.scale, banner.height() - 4.0 * self.scale),
            );
            let r = ui.interact(uncut, base.with((col_idx, "uncut")), egui::Sense::click());
            p.rect_filled(uncut, 3.0, if r.hovered() { th.sel_bg } else { th.bg });
            p.text(
                uncut.center(),
                egui::Align2::CENTER_CENTER,
                "Uncut",
                egui::FontId::proportional(10.5 * self.scale),
                th.text,
            );
            if r.on_hover_text("Return these cards to Current Done").clicked() {
                self.acts.push(BoardAct::Uncut(v));
            }
            body_top += QUICK_ADD_H * self.scale;
        }
```

- [ ] **Step 6: The detail page**

In `show_details`, directly after the `ui.label(RichText::new(format!("{} · {}", card.id, column_title(card.state))).color(th.dim));` statement:

```rust
                #[cfg(test)]
                {
                    self.detail_commits = card
                        .shipped
                        .as_ref()
                        .map(|s| s.commits.clone())
                        .unwrap_or_default();
                }
                if let Some(s) = &card.shipped {
                    ui.label(
                        egui::RichText::new(format!("Version: {} · cut {}", s.name, s.at))
                            .color(th.dim),
                    );
                }
```

After the Description block (`detail_text(ui, card.body...)`), before the `if let Some(reason) = &card.blocked_reason` block:

```rust
                if let Some(s) = &card.shipped
                    && !s.commits.is_empty()
                {
                    ui.add_space(12.0 * self.scale);
                    ui.strong("Commits");
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(s.commits.join("  ")).monospace(),
                        )
                        .wrap()
                        .selectable(true),
                    );
                }
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test --target-dir target/agent board::tests 2>&1 | tail -5`
Expected: all board tests pass, including the four new ones. If `cut_button_opens_the_field_prefill_lands_and_enter_records_the_act` fails on the Enter step only, check that the TextEdit had focus on the frame before the key event (the `cut_focus_pending` frame is the click's release frame; add one empty `run_frame` before the Enter frame if egui defers the focus request by a frame).

- [ ] **Step 8: Build and ask for a screenshot**

Run: `cargo build --target-dir target/agent`. Then ask the user to run **build-screenshot** with the board open, once in Current with a Done card (Cut visible, dropdown reads Current) and once with a Version selected (banner, Uncut, no Cut). Screenshot evidence is additional to the tests above, not a substitute.

- [ ] **Step 9: Commit**

```bash
git add src/board.rs
git commit -m "feat(board): Done version dropdown, Cut field, archive banner, shipped card details"
```

---

### Task 7: Docs, vocabulary, and the embedded skill

**Files:**
- Modify: `docs/kanban-board.md`
- Modify: `CONTEXT.md`
- Modify: `.claude/skills/foreman-kanban/SKILL.md`, `.codex/skills/foreman-kanban/SKILL.md`

**Interfaces:**
- Consumes: the shipped behaviour of Tasks 1 to 6. No code.

- [ ] **Step 1: `docs/kanban-board.md`**

In **What it does**, after the **Worktree status is derived** bullet, add:

```markdown
- **Cut and Versions**: Done is the live pile until you Cut it. Cut (a
  button on the Done header, or `foreman kanban cut <name>`) stamps every
  ungrouped Done card with `shipped` (`{name, at, commits}`), which moves
  them out of Current into a named Version; `state` stays `done`. The Done
  header's dropdown switches between Current and any Version, newest Cut
  first; a Version shows an `Archived · <name>` banner with Uncut, hides
  Cut, and leaves the other three columns live. A Done card whose kept
  worktree is still ahead of base is not in the tip, so Cut leaves it in
  Current and says so; merge or Discard it and it goes into the next Cut.
  Duplicate names (case-insensitive) and `Current` are refused. The
  selection is view state and resets to Current on restart. Why this shape
  and what was rejected: `docs/superpowers/specs/2026-09-16-kanban-cut-design.md`.
- **Commits attach at Cut through the card trailer.** Every dispatch prompt
  tells the worker to end each commit message with `Card: <id>`. Cut walks
  `git log` once (bounded by the oldest card's creation date) and stores
  each card's trailer commits in `shipped.commits`, shown on the detail
  page and in `list --json`. A card whose commits lack the trailer ships
  with none; nothing is ever refreshed after Cut.
```

In the CLI block, replace the `list` line and add two lines after `rm`:

```
foreman kanban list [--state ...] [--shipped NAME] [--all] [--json]   # bare = live board
foreman kanban cut <name>                 # ship ritual: Done -> Version <name>
foreman kanban uncut <name>               # Version <name> -> Current Done
```

In **Transitions**, replace `Done is terminal — delete or promote to a GitHub issue.` with `Done is terminal for state; Cut and Uncut group and ungroup Done cards without changing it.`

In **Gotchas**, add:

```markdown
- **Cut on the branch you ship from.** A Cut rewrites every ungrouped Done
  card file at once; two branches each cutting overlapping cards conflict
  per file on merge, like any two transitions on one card would.
- **A Cut with no git, or before any trailer commit, still ships.** It just
  records no commits. The trailer is a convention the prompt teaches, not a
  gate `done` enforces.
- **Bare `list` hides shipped cards.** Scripts that dumped every card need
  `--all`; `--state done` is Current Done only.
```

In **Key files**, extend the `src/kanban.rs` bullet with `; the Cut half: `Shipped`, `same_name`, `versions`, `CardStore::cut` / `uncut` (batch write, revert), `parse_trailer_log` + `trailer_commits`, `latest_v_tag``; the `src/board.rs` bullet with `, the Done header's version dropdown and Cut field, the archive banner`; the `src/wm.rs` bullet with `, `kanban_cut` (the hold-back probe and trailer walk injected into the store)`.

- [ ] **Step 2: `CONTEXT.md`**

In the **Card** entry, append to the definition: `A Done Card may be grouped into a Version by a Cut.`

After the **Board** entry, add:

```markdown
**Cut**:
The explicit action (Board button, `foreman kanban cut`) that stamps every
ungrouped Done Card as shipped in a named Version. Grouping, not a state
change.
_Avoid_: release (taken — that is In Progress/Blocked → Backlog), archive,
milestone, sprint.

**Version**:
The named group a Cut creates; the Done column can show Current or any one
Version. Exists only as the distinct `shipped.name` values on Cards.
_Avoid_: changelog, milestone, ship (the event, not the group).

**Card trailer**:
The `Card: <id>` line a Worker ends each commit message with; how a Cut
finds a Card's commits after rebase or squash.
_Avoid_: tag, footer, reference.
```

- [ ] **Step 3: Both skill copies**

In `.claude/skills/foreman-kanban/SKILL.md` **Verbs** block, replace the `list` line and add two lines:

```
    & $env:FOREMAN_EXE kanban list --shipped v0.5.0 --json
    & $env:FOREMAN_EXE kanban cut v0.5.0
    & $env:FOREMAN_EXE kanban uncut v0.5.0
```

In the `list` bullet, append: `Bare `list` is the live board — cards Cut into a Version are hidden; `--shipped NAME` lists one Version, `--all` everything.`

After the `wait` bullet add:

```markdown
- `cut NAME` / `uncut NAME` — the ship ritual, run by a human or a release
  script after tagging: `cut` moves every ungrouped Done card into Version
  NAME; `uncut` puts them back. Workers are not expected to Cut.
```

In **Close-out discipline**, add a paragraph after the first:

```markdown
End every commit message with the trailer line `Card: <id>` (your dispatch
prompt shows it). That line is how the board attaches your commits to the
card when the release is Cut; a commit without it is simply not attached.
```

Make the same three edits in `.codex/skills/foreman-kanban/SKILL.md`, in its fenced-block style (the `list`/`cut`/`uncut` lines go inside the existing ```powershell block) and its "Codex" phrasing (`Codex is not expected to Cut`).

- [ ] **Step 4: Rebuild so the embed propagates, and run the skill install test**

Run: `cargo test --target-dir target/agent skills_install 2>&1 | tail -5`
Expected: pass (the embedded copies are `include_str!`, so a rebuild is the propagation).

- [ ] **Step 5: Commit**

```bash
git add docs/kanban-board.md CONTEXT.md .claude/skills/foreman-kanban/SKILL.md .codex/skills/foreman-kanban/SKILL.md
git commit -m "docs(kanban): Cut, Versions, and the card trailer"
```

---

## Self-review

**Spec coverage.** Card schema (`{name, at, commits}`, omit-when-none, empty-name repair): Task 1. Trailer in both prompt renderings: Task 3. Cut steps 1 to 9 (reload, name rules, non-empty Done, duplicate, hold-back probe, bounded trailer walk, one stamp, batch write with revert, held-back lines): Tasks 2 and 5. Prefill from the unused newest `v*` tag: Tasks 3, 5, 6. Uncut (case-insensitive, no confirm): Tasks 2, 5, 6. Board UI (dropdown order, Cut only in Current, banner with Uncut, no Cut inside a Version, detail page Version and Commits, collapsed rail label, selection survives collapse, snap-back, controls do not collapse the column): Task 6. List and wire (`name`, `all`, bare `list` live, `--state done` Current, `--shipped` case-insensitive and empty on unknown, `--all` rules, `[shipped NAME]` tail, json object): Tasks 1, 4, 5. Transition rules unchanged: no task touches `done`/`release`/`rm`, and Task 2's `clear_worktree_on_a_shipped_card_keeps_shipped` pins the teardown rule. Docs: Task 7; help: Task 4. Every test row in the spec's Tests section maps to a named test above.

**Placeholder scan.** None. Every step has its code or exact text.

**Type consistency.** `Shipped { name, at, commits }` everywhere; `CutOutcome { name, shipped, held_back }` with `lines()`; `CardStore::cut(name, hold, commits)` where `hold: impl Fn(&Card) -> Option<String>` and `commits: impl FnOnce(&[Card]) -> HashMap<String, Vec<String>>`; `uncut(name) -> Result<usize, String>`; `trailer_commits(cwd, since) -> HashMap` and `latest_v_tag(cwd) -> Option<String>`; `KanbanRequest.name` / `.all`; `BoardAct::Cut(String)` / `Uncut(String)` / `CutPrefill`; `BoardView::prefill_cut(&mut self, &str)`; probes `offered_cut`, `drew_banner`, `done_listed`, `detail_commits`; constants `DD_W`, `CUT_W`. Task 5 declares the `prefill_cut` stub that Task 6 replaces, so each task compiles on its own.
