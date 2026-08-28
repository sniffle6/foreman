# Sessions-Panel Drag-and-Drop Reordering Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the user drag panel rows/chips to reorder Projects and, within a Project, its Session/chat/image rows — presentation-only, persisted in `workspace.json`.

**Architecture:** Ordering is panel presentation state owned and validated by `WindowManager`. A per-`Tab` optional rank (`panel_order: Option<u64>`) travels with the tab through merge/untab/capture/restore. `panel_model()` projects rows in rank order (unranked after ranked, structural tie-break). The drag gesture lives in `PanelView` and drains into one Deferred `Act::ReorderPanel`; the manager resolves source/anchor by **stable content identity** (`term_id` / project `tag`), validates scope, and rewrites dense ranks for that scope only. Never mutates the layout tree, `Win` z-order, real tab-strip order, focus, or active tabs.

**Tech Stack:** Rust, egui 0.34.3 (immediate mode), serde (workspace.json v1, additive fields only).

**Spec:** The "Design summary" section below (reviewed and approved 2026-08-27; supersedes the temp handoff `foreman-session-panel-reorder-plan-review-handoff.md`).

## Design summary (the approved spec)

- **Product contract (user-confirmed):** panel-only ordering. Dragging a panel row never moves tiles, tab strips, z-order, or focus. Cross-Project Session drops are visibly invalid (no insertion marker) and mutate nothing — ownership migration is out of scope.
- **Rank representation (correction 2):** `Option<u64>` on BOTH the live `Tab` and `TabSnap`. `None` = unranked → sorts after ranked rows in structural order. This makes every `Tab` creation site zero-touch: a missed site lands at the end (benign), never jumps to the front. Projection is one stable `sort_by_key(rank.unwrap_or(u64::MAX))`.
- **Drag-staleness guard (correction 1):** no rank tokens. Source/anchor resolve by the identities the codebase already routes by: `Session::term_id()` for terminal rows, nested-manager `tag` (`"pN"`) for Project rows. Chat/Image rows have no stable id → strict `TargetPath` + content-kind check; any drift cancels the drop (a cancelled drag, never a wrong-row move).
- **Normalization (correction 3):** on reorder mutation only. Restore stays read-only; projection is a pure sort. Each applied reorder rewrites the whole affected scope to dense ranks `0..n` (this also folds unranked rows in).
- **Scopes:** Project rows form one scope (all `Content::Project` tabs across desktop windows). Session rows form one scope per Project (all eligible tabs across that Project's child windows). Project↔Session and cross-Project drops are rejected.
- **Dirty:** an applied reorder marks the workspace dirty via the existing unconditional `mark_workspace_dirty()` after the act-apply loop (`src/wm.rs`, "over-dirty is intentional"). Deliberate deviation from the handoff's "no dirty on no-op": rejected/no-op acts may over-dirty; the repo's policy explicitly tolerates this and the view suppresses the common no-ops (self-drop) before recording an intent.
- **Gesture:** rows/chips draggable in expanded vertical, horizontal-columns, and strip modes. Collapsed rails stay non-draggable (v1). Click-below-threshold still surfaces; a completed drag never surfaces. Insertion marker: 2px line at the drop boundary. Edge auto-scroll drives the existing `PanelView::scroll` on the current `ScrollAxis`. Drag cancels if the panel collapses, the orientation flips, or the pointer state is lost.

## Global Constraints

- **⚠ If `$env:FOREMAN` is `1` you are running inside foreman.** Never `Stop-Process foreman`. All builds/tests: `cargo build --target-dir target/agent` / `cargo test --target-dir target/agent`.
- No new dependencies.
- `WORKSPACE_VERSION` stays `1`. New snapshot fields are additive: `#[serde(default, skip_serializing_if = "Option::is_none")]` so unset fields are omitted from the file.
- Never mutate the layout tree, `Win.z`, `Win.tabs` order, `Win` vec order, focus, or `active` from reorder code.
- New egui interaction Ids derive from `base` (never a bare `WinId`).
- Stage files **by name** (`git add src/wm.rs src/panel.rs …`); never `git add -A`. Commit with multiple `-m` args (no PowerShell here-strings — the `@` incident). Verify each commit with `git -C "H:/claude code/foreman" log -1 --format=%B`.
- Commit trailer on every commit (as separate final `-m` args):
  `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_017kGBkCC3PB93rSWvrXKmf8`
- GUI behavior claims need image evidence. The final task hands verification to the user (**build-screenshot** is user-run only).
- Serena note for implementers: line numbers below are approximate anchors; resolve by symbol name (`rg -n "fn <symbol>" src/<file>.rs`).

---

### Task 1: Persisted per-tab rank (`Tab.panel_order` + `TabSnap.panel_order`)

**Files:**
- Modify: `src/wm.rs` (struct `Tab`, `Tab::fixed`, `Tab::shell_default`, `WindowManager::capture_manager`, `WindowManager::apply_manager`)
- Modify: `src/workspace.rs` (struct `TabSnap`, `impl Default for TabSnap`)
- Test: `src/workspace.rs` `mod tests`, `src/wm.rs` `mod tests`

**Interfaces:**
- Consumes: existing `Tab`, `TabSnap`, `capture_manager`, `apply_manager`.
- Produces: `Tab.panel_order: Option<u64>` (pub field, defaults `None` in both constructors); `TabSnap.panel_order: Option<u64>` (serde-additive); capture copies live → snap; apply copies snap → live. Later tasks rely on the field name `panel_order` exactly.

- [ ] **Step 1: Write the failing tests**

In `src/workspace.rs` `mod tests`:

```rust
#[test]
fn tab_snap_panel_order_is_wire_compat_with_v1() {
    // A v1 tab (no panel_order key) still parses → None.
    let mut v1 = serde_json::to_value(TabSnap::default()).unwrap();
    v1.as_object_mut().unwrap().remove("panel_order");
    let snap: TabSnap = serde_json::from_value(v1).expect("v1 tab parses");
    assert_eq!(snap.panel_order, None);
    // Unset rank serializes away — old builds see byte-identical tabs.
    let out = serde_json::to_string(&snap).unwrap();
    assert!(!out.contains("panel_order"), "None must be omitted: {out}");
    // A set rank round-trips.
    let mut ranked = snap.clone();
    ranked.panel_order = Some(7);
    let back: TabSnap =
        serde_json::from_str(&serde_json::to_string(&ranked).unwrap()).unwrap();
    assert_eq!(back.panel_order, Some(7));
}
```

In `src/wm.rs` `mod tests` (model the `Win` literal on the one in `apply_manager`; the empty-Project-stub pattern is the sanctioned PTY-free stand-in):

```rust
fn ranked_proj_win(wm: &mut WindowManager, rank: Option<u64>, tag: &str) {
    let mut inner = WindowManager::new();
    inner.cwd = Some(std::env::temp_dir()); // restore skips projects without a real cwd
    inner.tag = Some(tag.into());
    let mut tab = Tab::fixed(tag, Content::Project(Box::new(inner)));
    tab.panel_order = rank;
    let id = wm.next;
    wm.next += 1;
    wm.z += 1;
    wm.windows.push(Win {
        id,
        tabs: vec![tab],
        active: 0,
        rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(80.0, 60.0)),
        z: wm.z,
        minimized: false,
        min_from_tree: false,
        prev: None,
    });
}

#[test]
fn panel_order_survives_capture_and_restore() {
    let ctx = egui::Context::default();
    let mut wm = WindowManager::new();
    ranked_proj_win(&mut wm, Some(2), "p1");
    ranked_proj_win(&mut wm, None, "p2");
    ranked_proj_win(&mut wm, Some(0), "p3");
    let snap = wm.capture_workspace();
    let mut fresh = WindowManager::new();
    fresh.apply_workspace(&snap, &ctx);
    // Fresh runtime ids, same ranks, same (z-ascending) capture order.
    let ranks: Vec<Option<u64>> = fresh
        .windows
        .iter()
        .flat_map(|w| w.tabs.iter().map(|t| t.panel_order))
        .collect();
    assert_eq!(ranks, vec![Some(2), None, Some(0)]);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --target-dir target/agent panel_order`
Expected: FAIL to compile — `panel_order` fields don't exist yet. (Compile failure is this cycle's red.)

- [ ] **Step 3: Implement the fields and plumbing**

`src/wm.rs`, struct `Tab` — add after `content`:

```rust
    /// Panel presentation rank: this tab's row position in the sessions panel.
    /// Presentation-only — never affects the real tab strip, layout tree, or
    /// z-order. `None` = unranked (sorts after ranked rows, structural order).
    /// Dense ranks are rewritten per scope on each panel reorder; the value
    /// travels with the tab through merge/untab/capture/restore.
    pub panel_order: Option<u64>,
```

Add `panel_order: None,` to both `Tab::fixed` and `Tab::shell_default` literals.

`src/workspace.rs`, struct `TabSnap` — add after `managed_title`:

```rust
    /// Panel presentation rank. Additive post-v1 field: omitted when unset so
    /// v1 files and old builds stay byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel_order: Option<u64>,
```

Add `panel_order: None,` to `impl Default for TabSnap`.

`src/wm.rs` `capture_manager` — in the `tabs.push(TabSnap { … })` literal add `panel_order: t.panel_order,`.

`src/wm.rs` `apply_manager` — the tab construction at the end of the per-tab loop becomes:

```rust
                let mut tab = match &content {
                    Content::Terminal(_) if managed => Tab::shell_default(restored_title, content),
                    _ => Tab::fixed(restored_title, content),
                };
                tab.panel_order = tab_snap.panel_order;
                tabs.push(tab);
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --target-dir target/agent panel_order`
Expected: both new tests PASS.

- [ ] **Step 5: Full-module regression + commit**

Run: `cargo test --target-dir target/agent workspace:: wm::` (then the full `cargo test --target-dir target/agent` if module filters pass).
Expected: green (pre-existing PTY tests can be flaky — rerun once before investigating).

```powershell
git add src/wm.rs src/workspace.rs
git commit -m "feat(panel): add persisted per-tab panel rank" -m "Option<u64> on Tab and TabSnap (serde-additive, omitted when unset; workspace stays v1). Capture/apply thread it through; both Tab constructors default None. Tests: v1 wire compat, capture/restore round-trip." -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>" -m "Claude-Session: https://claude.ai/code/session_017kGBkCC3PB93rSWvrXKmf8"
git -C "H:/claude code/foreman" log -1 --format=%B
```

---

### Task 2: Ordered projection + row identity in `panel_model()`

**Files:**
- Modify: `src/panel.rs` (structs `TabEntry`, `ProjectEntry`; new enum `RowIdentity`)
- Modify: `src/wm.rs` (`WindowManager::panel_model`)
- Test: `src/wm.rs` `mod tests`

**Interfaces:**
- Consumes: `Tab.panel_order` from Task 1; existing `panel_model` traversal.
- Produces:
  - `pub enum RowIdentity { Project(Option<String>), Terminal(u64), Loose }` (derives `Clone, PartialEq, Eq, Debug`) in `src/panel.rs`.
  - `TabEntry.rank: Option<u64>`, `TabEntry.identity: RowIdentity`; `ProjectEntry.rank: Option<u64>`, `ProjectEntry.identity: RowIdentity`.
  - `panel_model()` returns projects sorted by rank, and each project's `tabs` sorted by rank (unranked last, structural tie-break). Task 3+ relies on these exact names.

- [ ] **Step 1: Write the failing tests**

In `src/wm.rs` `mod tests` (reuse `ranked_proj_win` from Task 1):

```rust
#[test]
fn panel_model_orders_projects_by_rank_unranked_last() {
    let mut wm = WindowManager::new();
    ranked_proj_win(&mut wm, Some(2), "p1");
    ranked_proj_win(&mut wm, None, "p2");
    ranked_proj_win(&mut wm, Some(0), "p3");
    let m = wm.panel_model();
    let titles: Vec<&str> = m.projects.iter().map(|p| p.title.as_str()).collect();
    assert_eq!(titles, ["p3", "p1", "p2"]);
}

#[test]
fn panel_model_orders_session_rows_by_rank_within_project() {
    let mut wm = WindowManager::new();
    ranked_proj_win(&mut wm, None, "p1");
    // Two PTY-free stub rows inside p1's nested manager, ranks None / Some(0).
    let Content::Project(inner) = &mut wm.windows[0].tabs[0].content else {
        unreachable!()
    };
    for (rank, title) in [(None, "a"), (Some(0), "b")] {
        let id = inner.next;
        inner.next += 1;
        inner.z += 1;
        let mut t = Tab::fixed(title, Content::Project(Box::new(WindowManager::new())));
        t.panel_order = rank;
        inner.windows.push(Win {
            id,
            tabs: vec![t],
            active: 0,
            rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(40.0, 30.0)),
            z: inner.z,
            minimized: false,
            min_from_tree: false,
            prev: None,
        });
    }
    let m = wm.panel_model();
    let rows: Vec<&str> = m.projects[0].tabs.iter().map(|t| t.title.as_str()).collect();
    assert_eq!(rows, ["b", "a"]); // ranked first, unranked structural after
    assert_eq!(m.projects[0].identity, crate::panel::RowIdentity::Project(Some("p1".into())));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --target-dir target/agent panel_model_orders`
Expected: FAIL to compile (`rank`/`identity` fields and `RowIdentity` missing).

- [ ] **Step 3: Implement**

`src/panel.rs` — new enum next to `TargetPath`:

```rust
/// Stable identity for a panel row, used to re-resolve a drag's source and
/// anchor at drop time without trusting multi-frame `TargetPath` indices.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RowIdentity {
    /// Nested manager tag ("pN"). `None` only for untagged test stubs, which
    /// fall back to strict-path resolution.
    Project(Option<String>),
    /// Session member id (`Session::term_id`) — stable across merge/untab.
    Terminal(u64),
    /// Chat/Image rows (and nested-Project test stand-ins): no stable id.
    /// Resolved by strict path + content-kind check; any drift cancels.
    Loose,
}
```

Add to `TabEntry` and `ProjectEntry` (both):

```rust
    /// Panel presentation rank (drives row order; `None` = unranked).
    pub rank: Option<u64>,
    /// Stable identity for drag-drop resolution across frames.
    pub identity: RowIdentity,
```

`src/wm.rs` `panel_model` — populate and sort:

- In the project loop, after `let pfocused = …`, capture identity from the matched `inner`: the `ProjectEntry` literal gains `rank: pt.panel_order, identity: RowIdentity::Project(inner.tag.clone()),`.
- In the tab loop, the `TabEntry` literal gains `rank: t.panel_order,` and:

```rust
                            identity: match &t.content {
                                Content::Terminal(s) => RowIdentity::Terminal(s.term_id()),
                                _ => RowIdentity::Loose,
                            },
```

- After both loops, before building `PanelModel`:

```rust
        // Rank-ordered projection: ranked ascending, unranked after in
        // structural order (stable sort supplies the tie-break).
        fn rank_key(r: Option<u64>) -> u64 {
            r.unwrap_or(u64::MAX)
        }
        for p in &mut projects {
            p.tabs.sort_by_key(|t| rank_key(t.rank));
        }
        projects.sort_by_key(|p| rank_key(p.rank));
```

(`use crate::panel::*;` at the top of `panel_model` already imports `RowIdentity`.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --target-dir target/agent panel_model`
Expected: the two new tests PASS **and** every pre-existing `panel_model`/panel test stays green (all-unranked models keep exact structural order via the stable sort — if an existing test breaks, the sort is not stable or the key is wrong; fix the code, not the test).

- [ ] **Step 5: Commit**

```powershell
git add src/panel.rs src/wm.rs
git commit -m "feat(panel): rank-ordered panel projection with row identity" -m "panel_model sorts projects and per-project rows by panel_order (stable sort; unranked after ranked in structural order). Entries carry RowIdentity (term_id / project tag / loose) for drop-time re-resolution." -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>" -m "Claude-Session: https://claude.ai/code/session_017kGBkCC3PB93rSWvrXKmf8"
git -C "H:/claude code/foreman" log -1 --format=%B
```

---

### Task 3: The reorder seam — `Act::ReorderPanel`, strict resolution, dense renumber

**Files:**
- Modify: `src/panel.rs` (new types `Placement`, `PanelRowRef`, `PanelReorder`, pure fn `splice_order`; `PanelView.reorder` field)
- Modify: `src/wm.rs` (`enum Act`, `drain_panel_acts`, act apply arm, new `resolve_panel_row` / `apply_panel_reorder` / `renumber_projects` / `renumber_session_rows`)
- Test: `src/panel.rs` `mod tests`, `src/wm.rs` `mod tests`

**Interfaces:**
- Consumes: `RowIdentity`, entry `rank`/`identity` (Task 2), `Tab.panel_order` (Task 1).
- Produces (Task 4/5 record intents against these exact shapes):

```rust
// src/panel.rs
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Placement { Before, After }

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PanelRowRef {
    pub path: TargetPath,
    pub identity: RowIdentity,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PanelReorder {
    pub source: PanelRowRef,
    pub anchor: PanelRowRef,
    pub placement: Placement,
}

pub fn splice_order<K: Copy + Eq>(
    items: &[(K, Option<u64>)],
    src: K,
    anchor: K,
    placement: Placement,
) -> Option<Vec<K>>;

// PanelView gains:
pub reorder: Option<PanelReorder>,

// src/wm.rs
Act::ReorderPanel(crate::panel::PanelReorder)
// pub(crate) for test reach; not part of any public API:
fn apply_panel_reorder(&mut self, r: &crate::panel::PanelReorder) -> bool;
```

- [ ] **Step 1: Write the failing pure-ordering tests**

In `src/panel.rs` `mod tests`:

```rust
#[test]
fn splice_order_moves_before_and_after() {
    let items = [("a", Some(0)), ("b", Some(1)), ("c", Some(2))];
    assert_eq!(
        splice_order(&items, "c", "a", Placement::Before).unwrap(),
        vec!["c", "a", "b"]
    );
    assert_eq!(
        splice_order(&items, "a", "c", Placement::After).unwrap(),
        vec!["b", "c", "a"]
    );
}

#[test]
fn splice_order_rejects_noops_and_missing_keys() {
    let items = [("a", Some(0)), ("b", Some(1))];
    assert!(splice_order(&items, "a", "a", Placement::Before).is_none()); // self-drop
    assert!(splice_order(&items, "a", "b", Placement::Before).is_none()); // adjacent no-op
    assert!(splice_order(&items, "b", "a", Placement::After).is_none()); // adjacent no-op
    assert!(splice_order(&items, "x", "a", Placement::Before).is_none()); // stale source
    assert!(splice_order(&items, "a", "x", Placement::After).is_none()); // stale anchor
}

#[test]
fn splice_order_folds_unranked_after_ranked() {
    // Display order is b (ranked) then a (unranked); moving a before b is real.
    let items = [("a", None), ("b", Some(5))];
    assert_eq!(
        splice_order(&items, "a", "b", Placement::Before).unwrap(),
        vec!["a", "b"]
    );
}
```

- [ ] **Step 2: Run to verify failure, then implement `splice_order` + the types**

Run: `cargo test --target-dir target/agent splice_order` → FAIL to compile.

`src/panel.rs` (types as in the Interfaces block above, plus):

```rust
/// Rank-splice: order `items` (given in structural order) by rank — unranked
/// last, stable — then move `src` to sit `placement` relative to `anchor`.
/// Returns the new key order, or `None` for a self-drop, an adjacent no-op,
/// or a missing key. Pure; the caller writes the dense ranks back.
pub fn splice_order<K: Copy + Eq>(
    items: &[(K, Option<u64>)],
    src: K,
    anchor: K,
    placement: Placement,
) -> Option<Vec<K>> {
    let mut order: Vec<(K, Option<u64>)> = items.to_vec();
    order.sort_by_key(|(_, r)| r.unwrap_or(u64::MAX)); // stable → structural ties
    let keys: Vec<K> = order.iter().map(|(k, _)| *k).collect();
    let si = keys.iter().position(|k| *k == src)?;
    let mut next = keys.clone();
    next.remove(si);
    let ai = next.iter().position(|k| *k == anchor)?; // anchor == src → None
    let at = match placement {
        Placement::Before => ai,
        Placement::After => ai + 1,
    };
    next.insert(at, src);
    (next != keys).then_some(next)
}
```

`PanelView`: add `pub reorder: Option<PanelReorder>,` and initialize `reorder: None,` in `PanelView::new` / `with_dock`.

Run: `cargo test --target-dir target/agent splice_order` → PASS.

- [ ] **Step 3: Write the failing manager-side tests**

In `src/wm.rs` `mod tests`:

```rust
fn row_ref(p: &crate::panel::ProjectEntry) -> crate::panel::PanelRowRef {
    crate::panel::PanelRowRef { path: p.path, identity: p.identity.clone() }
}

fn tab_ref(t: &crate::panel::TabEntry) -> crate::panel::PanelRowRef {
    crate::panel::PanelRowRef { path: t.path, identity: t.identity.clone() }
}

#[test]
fn reorder_projects_renumbers_scope_dense() {
    use crate::panel::{PanelReorder, Placement};
    let mut wm = WindowManager::new();
    ranked_proj_win(&mut wm, None, "p1");
    ranked_proj_win(&mut wm, None, "p2");
    ranked_proj_win(&mut wm, None, "p3");
    let m = wm.panel_model();
    let r = PanelReorder {
        source: row_ref(&m.projects[2]), // p3
        anchor: row_ref(&m.projects[0]), // p1
        placement: Placement::Before,
    };
    assert!(wm.apply_panel_reorder(&r));
    let m = wm.panel_model();
    let titles: Vec<&str> = m.projects.iter().map(|p| p.title.as_str()).collect();
    assert_eq!(titles, ["p3", "p1", "p2"]);
    // First mutation normalizes the whole scope to dense ranks.
    let ranks: Vec<Option<u64>> = m.projects.iter().map(|p| p.rank).collect();
    assert_eq!(ranks, vec![Some(0), Some(1), Some(2)]);
}

#[test]
fn reorder_session_rows_within_a_project() {
    use crate::panel::{PanelReorder, Placement};
    let mut wm = WindowManager::new();
    ranked_proj_win(&mut wm, None, "p1");
    let Content::Project(inner) = &mut wm.windows[0].tabs[0].content else {
        unreachable!()
    };
    for title in ["a", "b", "c"] {
        let id = inner.next;
        inner.next += 1;
        inner.z += 1;
        inner.windows.push(Win {
            id,
            tabs: vec![Tab::fixed(title, Content::Project(Box::new(WindowManager::new())))],
            active: 0,
            rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(40.0, 30.0)),
            z: inner.z,
            minimized: false,
            min_from_tree: false,
            prev: None,
        });
    }
    let m = wm.panel_model();
    let r = PanelReorder {
        source: tab_ref(&m.projects[0].tabs[2]), // c
        anchor: tab_ref(&m.projects[0].tabs[0]), // a
        placement: Placement::Before,
    };
    assert!(wm.apply_panel_reorder(&r));
    let m = wm.panel_model();
    let rows: Vec<&str> = m.projects[0].tabs.iter().map(|t| t.title.as_str()).collect();
    assert_eq!(rows, ["c", "a", "b"]);
}

#[test]
fn reorder_rejects_cross_project_and_mixed_and_stale() {
    use crate::panel::{PanelReorder, PanelRowRef, Placement, RowIdentity};
    let mut wm = WindowManager::new();
    ranked_proj_win(&mut wm, None, "p1");
    ranked_proj_win(&mut wm, None, "p2");
    for w in 0..2 {
        let Content::Project(inner) = &mut wm.windows[w].tabs[0].content else {
            unreachable!()
        };
        let id = inner.next;
        inner.next += 1;
        inner.z += 1;
        inner.windows.push(Win {
            id,
            tabs: vec![Tab::fixed("t", Content::Project(Box::new(WindowManager::new())))],
            active: 0,
            rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(40.0, 30.0)),
            z: inner.z,
            minimized: false,
            min_from_tree: false,
            prev: None,
        });
    }
    let m = wm.panel_model();
    // Session → Session across projects: reject.
    let cross = PanelReorder {
        source: tab_ref(&m.projects[0].tabs[0]),
        anchor: tab_ref(&m.projects[1].tabs[0]),
        placement: Placement::Before,
    };
    assert!(!wm.apply_panel_reorder(&cross));
    // Project → Session mix: reject.
    let mixed = PanelReorder {
        source: row_ref(&m.projects[0]),
        anchor: tab_ref(&m.projects[1].tabs[0]),
        placement: Placement::Before,
    };
    assert!(!wm.apply_panel_reorder(&mixed));
    // Stale identity: reject.
    let stale = PanelReorder {
        source: PanelRowRef {
            path: m.projects[0].path,
            identity: RowIdentity::Project(Some("p99".into())),
        },
        anchor: row_ref(&m.projects[1]),
        placement: Placement::Before,
    };
    assert!(!wm.apply_panel_reorder(&stale));
    // Nothing mutated by any rejection.
    let after = wm.panel_model();
    assert!(after.projects.iter().all(|p| p.rank.is_none()));
    assert!(after.projects.iter().flat_map(|p| &p.tabs).all(|t| t.rank.is_none()));
}

#[test]
fn panel_rank_travels_through_tab_merge() {
    use crate::panel::{PanelReorder, Placement};
    let mut wm = WindowManager::new();
    ranked_proj_win(&mut wm, None, "p1");
    let Content::Project(inner) = &mut wm.windows[0].tabs[0].content else {
        unreachable!()
    };
    for title in ["a", "b"] {
        let id = inner.next;
        inner.next += 1;
        inner.z += 1;
        inner.windows.push(Win {
            id,
            tabs: vec![Tab::fixed(title, Content::Project(Box::new(WindowManager::new())))],
            active: 0,
            rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(40.0, 30.0)),
            z: inner.z,
            minimized: false,
            min_from_tree: false,
            prev: None,
        });
    }
    // Rank b before a, then merge b's window onto a's — order must survive.
    let m = wm.panel_model();
    let r = PanelReorder {
        source: tab_ref(&m.projects[0].tabs[1]), // b
        anchor: tab_ref(&m.projects[0].tabs[0]), // a
        placement: Placement::Before,
    };
    assert!(wm.apply_panel_reorder(&r));
    let Content::Project(inner) = &mut wm.windows[0].tabs[0].content else {
        unreachable!()
    };
    let (a_id, b_id) = (inner.windows[0].id, inner.windows[1].id);
    inner.merge_windows(b_id, a_id);
    let m = wm.panel_model();
    let rows: Vec<&str> = m.projects[0].tabs.iter().map(|t| t.title.as_str()).collect();
    assert_eq!(rows, ["b", "a"]);
}
```

Run: `cargo test --target-dir target/agent reorder_ panel_rank_travels` → FAIL to compile (`apply_panel_reorder` missing).

- [ ] **Step 4: Implement the manager side**

`src/wm.rs` — `enum Act`, add a variant:

```rust
    /// Panel drag-drop reorder: presentation-only rank rewrite. Never touches
    /// the tree, z-order, tab-strip order, focus, or active tabs. Deferred:
    /// renumbering needs `&mut` across windows while the panel view's render
    /// borrow is live.
    ReorderPanel(crate::panel::PanelReorder),
```

`drain_panel_acts` — add alongside `click`/`hover`:

```rust
        let mut reorder = None;
        // …inside the Content::TaskManager(v) arm:
                    if let Some(r) = v.reorder.take() {
                        reorder = Some(r);
                    }
        // …after the loop, with the other pushes:
        if let Some(r) = reorder {
            acts.push(Act::ReorderPanel(r));
        }
```

Act apply pass (the `match a { … }` alongside `Act::FocusPath` etc.):

```rust
                Act::ReorderPanel(r) => {
                    // Rejects are no-ops; the trailing mark_workspace_dirty()
                    // over-dirties by design (see the comment below the loop).
                    self.apply_panel_reorder(&r);
                }
```

New methods on `WindowManager` (place near `panel_model`):

```rust
    /// A panel row resolved to live structure. Indices are only valid until
    /// the next structural mutation — resolve and use within one act.
    /// Values are indices into `self.windows` / `.tabs` (and, for Session
    /// rows, into the owning project's nested manager).
    enum ResolvedRow {
        Project { win: usize, tab: usize },
        Session { pwin: usize, ptab: usize, cwin: usize, tab: usize },
    }
```

(Declare `ResolvedRow` as a private module-level enum in `src/wm.rs`, not inside the impl.)

```rust
    /// Resolve a drag ref by stable identity (identity-first; strict path only
    /// for Loose rows). Returns None on any drift — a cancelled drop, never a
    /// wrong-row move.
    fn resolve_panel_row(&self, r: &crate::panel::PanelRowRef) -> Option<ResolvedRow> {
        use crate::panel::RowIdentity;
        match &r.identity {
            RowIdentity::Project(Some(tag)) => {
                for (wi, w) in self.windows.iter().enumerate() {
                    for (pi, t) in w.tabs.iter().enumerate() {
                        if let Content::Project(inner) = &t.content {
                            if inner.tag.as_deref() == Some(tag.as_str()) {
                                return Some(ResolvedRow::Project { win: wi, tab: pi });
                            }
                        }
                    }
                }
                None
            }
            RowIdentity::Project(None) => {
                // Untagged stub (tests only): strict path.
                let wi = self.windows.iter().position(|w| w.id == r.path.project)?;
                let pi = r.path.tab?;
                let t = self.windows[wi].tabs.get(pi)?;
                matches!(t.content, Content::Project(_))
                    .then_some(ResolvedRow::Project { win: wi, tab: pi })
            }
            RowIdentity::Terminal(tid) => {
                for (wi, w) in self.windows.iter().enumerate() {
                    for (pi, t) in w.tabs.iter().enumerate() {
                        let Content::Project(inner) = &t.content else { continue };
                        for (ci, cw) in inner.windows.iter().enumerate() {
                            for (ti, ct) in cw.tabs.iter().enumerate() {
                                if let Content::Terminal(s) = &ct.content {
                                    if s.term_id() == *tid {
                                        return Some(ResolvedRow::Session {
                                            pwin: wi,
                                            ptab: pi,
                                            cwin: ci,
                                            tab: ti,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
                None
            }
            RowIdentity::Loose => {
                // Chat/Image rows: strict path + kind family check.
                let wi = self.windows.iter().position(|w| w.id == r.path.project)?;
                let pi = r.path.ptab?;
                let Content::Project(inner) = &self.windows[wi].tabs.get(pi)?.content else {
                    return None;
                };
                let cid = r.path.window?;
                let ci = inner.windows.iter().position(|w| w.id == cid)?;
                let ti = r.path.tab?;
                let ct = inner.windows[ci].tabs.get(ti)?;
                (!matches!(ct.content, Content::TaskManager(_) | Content::Settings(_)
                    | Content::Terminal(_)))
                .then_some(ResolvedRow::Session { pwin: wi, ptab: pi, cwin: ci, tab: ti })
            }
        }
    }

    /// Validate + apply one panel reorder. Returns false (and mutates nothing)
    /// for stale refs, scope mixes, cross-project drops, and no-ops.
    pub(crate) fn apply_panel_reorder(&mut self, r: &crate::panel::PanelReorder) -> bool {
        let (src, anchor) =
            match (self.resolve_panel_row(&r.source), self.resolve_panel_row(&r.anchor)) {
                (Some(s), Some(a)) => (s, a),
                _ => return false,
            };
        match (src, anchor) {
            (ResolvedRow::Project { win: sw, tab: st }, ResolvedRow::Project { win: aw, tab: at }) => {
                self.renumber_projects((sw, st), (aw, at), r.placement)
            }
            (
                ResolvedRow::Session { pwin: sp, ptab: spt, cwin: sc, tab: st },
                ResolvedRow::Session { pwin: ap, ptab: apt, cwin: ac, tab: at },
            ) if sp == ap && spt == apt => {
                let Content::Project(inner) = &mut self.windows[sp].tabs[spt].content else {
                    return false;
                };
                inner.renumber_session_rows((sc, st), (ac, at), r.placement)
            }
            // Project↔Session mixes and cross-project Session drops: invalid.
            _ => false,
        }
    }

    /// Dense-renumber the desktop's Project-tab scope after a splice.
    fn renumber_projects(
        &mut self,
        src: (usize, usize),
        anchor: (usize, usize),
        placement: crate::panel::Placement,
    ) -> bool {
        let mut items: Vec<((usize, usize), Option<u64>)> = Vec::new();
        for (wi, w) in self.windows.iter().enumerate() {
            for (pi, t) in w.tabs.iter().enumerate() {
                if matches!(t.content, Content::Project(_)) {
                    items.push(((wi, pi), t.panel_order));
                }
            }
        }
        let Some(next) = crate::panel::splice_order(&items, src, anchor, placement) else {
            return false;
        };
        for (rank, (wi, pi)) in next.into_iter().enumerate() {
            self.windows[wi].tabs[pi].panel_order = Some(rank as u64);
        }
        true
    }

    /// Dense-renumber this (nested) manager's eligible-tab scope. Eligibility
    /// mirrors `panel_model`: everything except TaskManager/Settings tabs.
    fn renumber_session_rows(
        &mut self,
        src: (usize, usize),
        anchor: (usize, usize),
        placement: crate::panel::Placement,
    ) -> bool {
        let mut items: Vec<((usize, usize), Option<u64>)> = Vec::new();
        for (ci, cw) in self.windows.iter().enumerate() {
            for (ti, t) in cw.tabs.iter().enumerate() {
                if !matches!(t.content, Content::TaskManager(_) | Content::Settings(_)) {
                    items.push(((ci, ti), t.panel_order));
                }
            }
        }
        let Some(next) = crate::panel::splice_order(&items, src, anchor, placement) else {
            return false;
        };
        for (rank, (ci, ti)) in next.into_iter().enumerate() {
            self.windows[ci].tabs[ti].panel_order = Some(rank as u64);
        }
        true
    }
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --target-dir target/agent reorder_ panel_rank_travels splice_order`
Expected: PASS.

- [ ] **Step 6: Full suite + commit**

Run: `cargo test --target-dir target/agent`
Expected: green (rerun known-flaky PTY tests once before investigating).

```powershell
git add src/panel.rs src/wm.rs
git commit -m "feat(wm): panel reorder act with identity resolution and dense ranks" -m "Act::ReorderPanel drains one PanelReorder intent per frame. Source/anchor resolve by stable identity (term_id / project tag; strict path for chat/image), scope-validated (project scope, per-project session scope; cross-project and mixed drops reject), then splice_order rewrites the scope to dense ranks. Rejects mutate nothing." -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>" -m "Claude-Session: https://claude.ai/code/session_017kGBkCC3PB93rSWvrXKmf8"
git -C "H:/claude code/foreman" log -1 --format=%B
```

---

### Task 4: Drag gesture — expanded vertical + horizontal columns

**Files:**
- Modify: `src/panel.rs` (`PanelView` fields, `PanelDrag` struct, `RowPaintOwned.drag_ref`, `paint_row` sense/lifecycle, target/marker computation in `show`'s vertical body and `paint_columns`, `insertion_at` + `drag_autoscroll` helpers)
- Test: `src/panel.rs` `mod tests` (pure geometry only; the gesture itself is GUI-verified in Task 6)

**Interfaces:**
- Consumes: `PanelRowRef`, `PanelReorder`, `Placement` (Task 3); entry `identity` (Task 2); existing `ScrollAxis`, `PanelView::scroll`.
- Produces:
  - `pub fn insertion_at(centers: &[f32], pointer: f32) -> Option<(usize, Placement)>` (pure, `src/panel.rs`).
  - `PanelView.drag: Option<PanelDrag>` (runtime-only; never persisted).
  - Rows in expanded vertical + columns modes start drags; a completed drop sets `PanelView.reorder` (drained by Task 3's wiring — no further wm changes).

- [ ] **Step 1: Write the failing pure-geometry test**

In `src/panel.rs` `mod tests`:

```rust
#[test]
fn insertion_at_resolves_slots_by_midpoint() {
    let centers = [10.0, 30.0, 50.0];
    assert_eq!(insertion_at(&centers, 5.0), Some((0, Placement::Before)));
    assert_eq!(insertion_at(&centers, 25.0), Some((1, Placement::Before)));
    assert_eq!(insertion_at(&centers, 40.0), Some((2, Placement::Before)));
    assert_eq!(insertion_at(&centers, 60.0), Some((2, Placement::After)));
    assert_eq!(insertion_at(&[], 10.0), None);
}
```

Run: `cargo test --target-dir target/agent insertion_at` → FAIL to compile.

- [ ] **Step 2: Implement `insertion_at` and the drag state**

`src/panel.rs`:

```rust
/// Insertion slot from same-scope row midpoints along the drag axis (midpoints
/// in display order): before the first row whose midpoint the pointer hasn't
/// passed, else after the last.
pub fn insertion_at(centers: &[f32], pointer: f32) -> Option<(usize, Placement)> {
    if centers.is_empty() {
        return None;
    }
    for (i, c) in centers.iter().enumerate() {
        if pointer < *c {
            return Some((i, Placement::Before));
        }
    }
    Some((centers.len() - 1, Placement::After))
}

/// Live drag-reorder gesture (expanded modes only). Runtime-only view state —
/// never persisted; cancelled on collapse or orientation change.
#[derive(Clone, Debug)]
pub struct PanelDrag {
    pub source: PanelRowRef,
    pub source_is_project: bool,
    /// Panel orientation at drag start; a mismatch on a later frame cancels.
    pub axis: ScrollAxis,
    /// Latest valid drop slot, recomputed each frame from same-scope rows.
    pub target: Option<(PanelRowRef, Placement)>,
    /// Insertion-marker segment to paint this frame (screen coords).
    pub marker: Option<(egui::Pos2, egui::Pos2)>,
}
```

`PanelView`: add `pub drag: Option<PanelDrag>,`; initialize `drag: None,` in `new` / `with_dock`.

`RowPaintOwned`: add field

```rust
    /// Some = this row is a drag-reorder source (expanded modes). Rail rows
    /// stay None/non-draggable.
    drag_ref: Option<PanelRowRef>,
```

Populate it at both `specs.push` sites (the vertical body in `show`, and the mirrored loop in `paint_columns`): project rows get `drag_ref: Some(PanelRowRef { path: proj.path, identity: proj.identity.clone() })`, tab rows `drag_ref: Some(PanelRowRef { path: t.path, identity: t.identity.clone() })`.

Run: `cargo test --target-dir target/agent insertion_at` → PASS.

- [ ] **Step 3: Drag lifecycle in `paint_row`**

`paint_row` gains an `axis: ScrollAxis` parameter (callers: `show` vertical body passes `ScrollAxis::Vertical`; `paint_columns` passes `ScrollAxis::Horizontal`). Change the row interact:

```rust
        let sense = if rp.drag_ref.is_some() {
            egui::Sense::click_and_drag()
        } else {
            egui::Sense::click()
        };
        let resp = ui.interact(row.intersect(clip), id, sense);
        if let Some(dr) = &rp.drag_ref {
            if resp.drag_started() {
                self.drag = Some(PanelDrag {
                    source: dr.clone(),
                    source_is_project: rp.project_row,
                    axis,
                    target: None,
                    marker: None,
                });
            }
        }
```

Keep the existing `resp.clicked()` → `self.click = Some(path)` handling untouched — egui only reports `clicked()` for sub-threshold presses, so click-to-surface survives and a completed drag never surfaces. The min/close buttons are registered **after** the row (on top), so presses over them go to the buttons; verify their clicks still land during Task 6's GUI pass — if a drag steals them, gate `drag_started` on the press position being outside the two button rects (`row.max.x - 34.0` is the left edge of the button cluster in vertical rows).

- [ ] **Step 4: Target + marker computation in the vertical body**

In `show`'s vertical mode, between building `specs` and the `paint_row` loop:

```rust
        // Drag target: same-scope rows only, resolved by Y midpoint.
        if let Some(d) = &mut self.drag {
            let scope_rows: Vec<(egui::Rect, PanelRowRef)> = specs
                .iter()
                .filter_map(|(rect, _, rp)| {
                    let r = rp.drag_ref.clone()?;
                    let same = if d.source_is_project {
                        rp.project_row
                    } else {
                        !rp.project_row
                            && r.path.project == d.source.path.project
                            && r.path.ptab == d.source.path.ptab
                    };
                    same.then_some((*rect, r))
                })
                .collect();
            d.target = None;
            d.marker = None;
            if let Some(ptr) = ui.ctx().pointer_latest_pos() {
                let centers: Vec<f32> = scope_rows.iter().map(|(r, _)| r.center().y).collect();
                if let Some((idx, placement)) = insertion_at(&centers, ptr.y) {
                    let (rect, anchor) = &scope_rows[idx];
                    let y = match placement {
                        Placement::Before => rect.min.y,
                        Placement::After => rect.max.y,
                    };
                    d.target = Some((anchor.clone(), placement));
                    d.marker = Some((egui::pos2(rect.min.x, y), egui::pos2(rect.max.x, y)));
                }
            }
        }
```

After the row loop, paint the marker and run auto-scroll (a cross-Project hover simply finds no same-scope slot under the pointer's nearest boundary — the marker still snaps to the source project's rows; the *absence* of a marker inside the hovered foreign project is the visible "invalid" signal):

```rust
        if let Some((a, b)) = self.drag.as_ref().and_then(|d| d.marker) {
            ui.painter().line_segment([a, b], egui::Stroke::new(2.0, th.text));
        }
        self.drag_autoscroll(ui, clip, ScrollAxis::Vertical, max_scroll);
```

(`th`, `clip`, and `max_scroll` are the local bindings already in scope in each mode's body; reuse the ones present.)

The auto-scroll helper on `PanelView`:

```rust
    /// While dragging near a clip edge, advance the existing scroll offset on
    /// the current axis and request a repaint (egui only repaints on input
    /// otherwise, and a held-still pointer generates none).
    fn drag_autoscroll(
        &mut self,
        ui: &egui::Ui,
        clip: egui::Rect,
        axis: ScrollAxis,
        max_scroll: f32,
    ) {
        const ZONE: f32 = 24.0;
        const SPEED: f32 = 420.0; // px/s
        if self.drag.is_none() {
            return;
        }
        let Some(p) = ui.ctx().pointer_latest_pos() else { return };
        let dt = ui.input(|i| i.stable_dt).min(0.05);
        let (pos, lo, hi) = match axis {
            ScrollAxis::Vertical => (p.y, clip.min.y, clip.max.y),
            ScrollAxis::Horizontal => (p.x, clip.min.x, clip.max.x),
        };
        let delta = if pos < lo + ZONE {
            -SPEED * dt
        } else if pos > hi - ZONE {
            SPEED * dt
        } else {
            0.0
        };
        if delta != 0.0 {
            self.scroll = (self.scroll + delta).clamp(0.0, max_scroll);
            ui.ctx().request_repaint();
        }
    }
```

- [ ] **Step 5: Completion / cancellation in `show`**

In `show`, after the mode dispatch (once per frame, whatever mode painted), with `axis` = the orientation `show` computed this frame:

```rust
        // Drag completion/cancellation — one place, after the mode painters
        // updated `target`. Collapse, orientation flip, or lost pointer state
        // cancels; a release over a valid slot records ONE reorder intent.
        if self.drag.is_some() {
            let (released, down) =
                ui.input(|i| (i.pointer.primary_released(), i.pointer.primary_down()));
            let flipped = self.drag.as_ref().is_some_and(|d| d.axis != axis);
            if self.collapsed || flipped || (!down && !released) {
                self.drag = None;
            } else if released {
                let d = self.drag.take().unwrap();
                if let Some((anchor, placement)) = d.target {
                    if anchor != d.source {
                        self.reorder = Some(PanelReorder {
                            source: d.source,
                            anchor,
                            placement,
                        });
                    }
                }
            }
        }
```

- [ ] **Step 6: Mirror in `paint_columns` (axis split per scope)**

Same wiring as Step 4 in `paint_columns`' row loop, with one difference per the spec: when `source_is_project`, resolve against project-header rows by **X midpoint** (`rect.center().x`, pointer `ptr.x`) and paint a **vertical** marker at `rect.min.x` / `rect.max.x` spanning `clip.min.y..clip.max.y`; when the source is a session row, resolve by **Y midpoint** among same-project rows exactly as vertical mode. Auto-scroll uses `ScrollAxis::Horizontal` with the columns' `clip` / `max_scroll`.

- [ ] **Step 7: Build + tests + commit**

Run: `cargo build --target-dir target/agent` then `cargo test --target-dir target/agent panel::`
Expected: clean build (existing warning baseline only), tests green. GUI verification is deferred to Task 6 — do not claim the gesture works.

```powershell
git add src/panel.rs
git commit -m "feat(panel): drag-reorder rows in vertical and columns modes" -m "Rows sense click_and_drag; drop records one PanelReorder intent (self-drop suppressed, adjacency no-ops rejected downstream). Same-scope midpoint targeting, 2px insertion marker, axis-aware edge auto-scroll on the existing scroll offset, cancel on collapse/orientation flip. Rails stay non-draggable." -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>" -m "Claude-Session: https://claude.ai/code/session_017kGBkCC3PB93rSWvrXKmf8"
git -C "H:/claude code/foreman" log -1 --format=%B
```

---

### Task 5: Strip-mode drag (inline chips)

**Files:**
- Modify: `src/panel.rs` (`paint_strip` chip specs + interact + target/marker + auto-scroll)

**Interfaces:**
- Consumes: `PanelDrag`, `insertion_at`, `drag_autoscroll`, `PanelRowRef` (Task 4/3); the strip's existing per-chip spec struct (local to `paint_strip`) and its `chip.id` / `chip.path` fields.
- Produces: strip chips reorder by X midpoint with Project-ownership boundaries; completion still flows through the shared handler in `show` (Task 4 Step 5).

- [ ] **Step 1: Extend the chip specs**

`paint_strip` builds a local list of chip specs before its paint loop. Extend that local struct with the same two facts rows carry: `drag_ref: Option<PanelRowRef>` (project chips → project identity; terminal/chat/image chips → tab identity, exactly as Task 4 Step 2 populated `RowPaintOwned`) and `project_chip: bool`.

- [ ] **Step 2: Sense + lifecycle on chips**

In the chip loop (the `ui.interact(chip_rect.intersect(content_rect), chip.id, egui::Sense::click())` site), switch to `Sense::click_and_drag()` when `drag_ref.is_some()`, and on `resp.drag_started()` set `self.drag` exactly as `paint_row` does (`source_is_project: chip.project_chip`, `axis: ScrollAxis::Horizontal`). Keep `resp.clicked()` → `self.click` untouched.

- [ ] **Step 3: Target + marker by X midpoint with ownership boundaries**

Before the chip paint loop, when `self.drag` is active: filter chips to the drag's scope — project chips for a project source; for a session source, only chips whose `path.project`/`path.ptab` match the source's (this is the Project-ownership boundary: foreign-project chips are simply not slots, so no marker appears over them). Feed `chip_rect.center().x` values to `insertion_at(…, ptr.x)`; marker is a vertical 2px line at `chip_rect.min.x` (Before) / `chip_rect.max.x` (After) spanning the strip line's height. Store `target`/`marker` on the drag; the shared completion handler in `show` does the rest.

After the loop: paint the marker (same `line_segment` as Task 4) and call `self.drag_autoscroll(ui, content_rect, ScrollAxis::Horizontal, max_scroll)` with the strip's own clip/max-scroll bindings.

- [ ] **Step 4: Build + tests + commit**

Run: `cargo build --target-dir target/agent` then `cargo test --target-dir target/agent panel::`
Expected: clean; no behavior claims until Task 6's GUI pass.

```powershell
git add src/panel.rs
git commit -m "feat(panel): drag-reorder chips in strip mode" -m "Strip chips sense click_and_drag; X-midpoint targeting within project-ownership boundaries, vertical insertion marker, shared completion/cancel and horizontal edge auto-scroll." -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>" -m "Claude-Session: https://claude.ai/code/session_017kGBkCC3PB93rSWvrXKmf8"
git -C "H:/claude code/foreman" log -1 --format=%B
```

---

### Task 6: Docs, reviewer gate, and verification

**Files:**
- Modify: `docs/task-manager-panel.md`, `docs/workspace-persistence.md`, `CONTEXT.md`

**Interfaces:**
- Consumes: everything shipped in Tasks 1–5.
- Produces: updated feature docs (no new doc file), a CONTEXT.md glossary entry, a foreman-reviewer pass, and the user-facing verification handoff.

- [ ] **Step 1: Update `docs/task-manager-panel.md`**

In "How it works", add one bullet after the write-seam bullet:

```markdown
- **Drag to reorder (presentation-only):** rows/chips in the expanded modes
  drag to reorder Project groups, and Session/chat/image rows within their
  Project. One `PanelReorder` intent drains into `Act::ReorderPanel`;
  `WindowManager` re-resolves source/anchor by stable identity (`term_id` /
  project `pN` tag; strict path for chat/image — drift cancels), rejects
  cross-project and Project↔Session drops, and rewrites that scope to dense
  per-tab `panel_order` ranks. The real tab strip, tiles, z-order, and focus
  never move. Unranked (`None`) rows sort after ranked ones in structural
  order, so new tabs append. A 2px marker shows the drop slot; edge
  auto-scroll follows the active `ScrollAxis`; collapse or an orientation
  flip cancels the gesture. Collapsed rails don't drag (v1).
```

Remove nothing from "Out of scope" (drag **into the tree** stays out of scope; it is a different feature). Add `panel_order` to the `src/wm.rs` line in "Key files" (`panel_model`, `apply_panel_reorder`, …).

- [ ] **Step 2: Update `docs/workspace-persistence.md`**

In "What it does", add a bullet:

```markdown
- Per-tab sessions-panel rank (`TabSnap.panel_order`, additive `Option` —
  omitted when unset, so v1 files and old builds are unaffected)
```

In "Gotchas", add:

```markdown
- **Panel ranks are `Option`.** Unranked tabs project after ranked ones in
  structural order; ranks are only normalized (dense per scope) when a panel
  reorder applies — restore never rewrites them. Pre-existing quirk: unranked
  rows can shuffle across a restart because capture orders windows by z;
  ranked rows are immune.
```

- [ ] **Step 3: CONTEXT.md glossary entry**

Add under the seams/patterns vocabulary (match the file's existing entry style):

```markdown
- **Panel order** — presentation-only per-tab rank (`Tab::panel_order`)
  driving sessions-panel row order. Written only by `Act::ReorderPanel`
  (dense per scope), projected by `panel_model()`, persisted additively in
  `TabSnap`. Never touches the tab strip, tree, z-order, or focus.
```

- [ ] **Step 4: foreman-reviewer pass**

Dispatch the **foreman-reviewer** agent over the full feature diff (`git diff <commit-before-task-1>..HEAD`) — it gates wm/panel/workspace changes per change control. Address findings before proceeding; re-run until clean or findings are explicitly waived by the user.

- [ ] **Step 5: Full validation**

Run: `cargo test --target-dir target/agent` (rerun once on known-flaky PTY tests) and `cargo build --target-dir target/agent`.
Expected: green build with only the documented warning baseline.

- [ ] **Step 6: Commit docs**

```powershell
git add docs/task-manager-panel.md docs/workspace-persistence.md CONTEXT.md
git commit -m "docs(panel): document sessions-panel drag reordering" -m "Feature-doc updates in place (no new doc), workspace persistence notes for the additive panel_order field, CONTEXT.md 'Panel order' entry." -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>" -m "Claude-Session: https://claude.ai/code/session_017kGBkCC3PB93rSWvrXKmf8"
git -C "H:/claude code/foreman" log -1 --format=%B
```

- [ ] **Step 7: Hand GUI verification to the user (required — no completion claim without it)**

The GUI cannot be seen from the terminal and **build-screenshot** is user-run only. Ask the user to run the `target/agent` build and verify, with screenshots:

1. Vertical expanded: drag a Session row within its Project (marker appears; drop reorders; click still surfaces; min/close buttons still click).
2. Drag a Project row across other Projects.
3. Drag a Session toward another Project → no marker inside the foreign Project, drop does nothing.
4. Bottom-docked columns mode: reorder a Project group (X-axis) and a Session row (Y-axis); strip mode: reorder chips.
5. Edge auto-scroll in a tall/wide overflowing panel.
6. Restart foreman → order survives (workspace.json round-trip).

Only after the user confirms (their hands-on test is the final gate) is the feature done.

---

## Self-review notes (already applied)

- Every type/function name cross-checked across tasks: `panel_order`, `RowIdentity`, `PanelRowRef`, `PanelReorder`, `Placement`, `splice_order`, `insertion_at`, `PanelDrag`, `drag_ref`, `apply_panel_reorder`, `renumber_projects`, `renumber_session_rows`, `Act::ReorderPanel`, `PanelView.{drag,reorder}`.
- Spec coverage: rank persistence (T1), ordered projection (T2), seam+validation+normalization (T3), vertical/columns gesture incl. marker/auto-scroll/cancel (T4), strip gesture (T5), rails-stay-static (T4 note), docs+verification (T6).
- Known judgment calls an implementer must not "fix": over-dirty on rejected acts (existing policy); `Option<u64>` instead of bare `u64` (spec correction 2); identity-first resolution instead of rank tokens (correction 1); restore never normalizes (correction 3).
