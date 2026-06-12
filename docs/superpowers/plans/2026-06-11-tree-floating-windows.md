# Tree + Floating Window States Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the 9-zone snap system with two window states — **tiled** (an i3-style layout tree of recursive splits) and **floating** — at both compositor levels.

**Architecture:** A new pure module `src/layout.rs` owns a `LayoutTree` (leaves = `WinId`s, internal nodes = H/V splits with ratios). Each `WindowManager` gets a `tree` field; a window is *tiled* iff its id is a tree leaf, *floating* otherwise. Tabs stay on `Win` (a leaf with tabs IS a tabbed container). Zoom becomes a tmux-style overlay (`zoomed: Option<WinId>`), leaving the tree intact. The whole `Zone` machinery (`Zone`, `detect_zone`, `zone_rect`, `compose_zone`, `interior_edges`, `resolve_zone`, `Win.snap`, `split: Vec2`, dwell/hold-to-max) is deleted at the end.

**Tech Stack:** Rust, egui 0.34, existing test harness (`cargo test --bin foreman`). No new dependencies.

---

## Settled design decisions (from the user conversation, 2026-06-11)

1. **Two states, both levels.** Tiled (tree) + floating, in the shared `WindowManager` engine → desktop projects and project terminals both get it.
2. **Tabs = a leaf whose `Win` has multiple tabs.** ⚠️ Divergence from the early sketch ("`Layout::Tabbed` node type"): the codebase already models a window AS a tab stack (`Win.tabs`/`Win.active`), with chat membership (`Tab.chat_member`), merge/untab, and ~30 tests built on it. A tree leaf pointing at a multi-tab `Win` delivers every agreed behavior (tabbed group inside any split; splitting from a tab splits the stack's pane). The only thing it can't express is a *tab containing a split subtree* — not needed. If that's ever wanted, `Node::Leaf` can become a node-with-layout later without redoing this work.
3. **Header drag = state transition.** Dragging a tiled window's header tears it out of the tree immediately (floating, follows cursor). While dragging any window: hovering a leaf edge shows a half-leaf insertion hint (split), hovering a leaf center shows a tab-merge hint, hovering the area edge band shows a root-split hint. Drop commits; drop on nothing → stays floating.
4. **Keyboard:** leader `WASD` = move window within the tree (swap with neighbor; no neighbor → move to area edge; floating → enter tree at edge). Leader `Alt+WASD` = split: new terminal splits the focused tiled leaf; a floating/absent source first enters the tree so you always get the two-pane result. New `leader F` / `leader Ctrl+F` = toggle float (terminal / project). Arrows (focus) unchanged — geometric focus works across both states.
5. **New windows default to tiled** (split the focused leaf along its longer axis; empty tree → first tile). The chat viewer window stays floating (it's a board, not a session).
6. **Zoom = tmux zoom.** `Z` / titlebar max / double-click renders the window full-area *on top*; the tree is untouched underneath; un-zoom restores instantly.
7. **Outer-edge resize of a tiled window is a no-op** (was: pop to floating). Tear-out now lives on the header drag; accidental float-on-resize was a misfeature. Interior edges drag the shared divider via tree ratios (arbitrary ratios — no more single `split: Vec2` for the whole manager).
8. **Hover-reveal headers: optional final task** (Task 16). ⚠️ It conflicts with the HANDOFF §5.1 roadmap (per-terminal status lines IN the titlebar). Ship the rework with persistent headers; the user decides on Task 16 separately.
9. `docs/epics/window-tabbing-split-epic.md` §1 explicitly chose *against* a tile tree. The user reversed that decision in conversation (2026-06-11). Task 15 marks the epic superseded so future sessions don't follow stale guidance.

## Branch / worktree

Executing on branch `feature/tiling-tree` in the MAIN checkout (`H:\claude code\foreman`). History: `feature/agent-dispatch` was fast-forward-merged into `main` (user decision, 2026-06-11) and this branch sits directly on the merged `main` (`0d8e580`), so Serena's project root and the working tree now agree — Serena symbol tools are safe to use again. The earlier worktree (`.claude/worktrees/tiling-tree`) is retired; its directory may linger until file locks release (gitignored, harmless). `docs/terminal-selection.md` remains untracked pending the selection-rewrite recovery.

## Build / test loop (Windows, PowerShell, GNU toolchain)

```powershell
# kill the app first or linking fails with "Access is denied (os error 5)"
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
cargo build 2>&1 | Select-Object -Last 20
cargo test --bin foreman 2>&1 | Select-Object -Last 20
```

GUI claims require a screenshot (Task 14; script in `docs/HANDOFF.md` §3). Serena tools are the required way to read/edit code symbols in this repo.

## File structure

- **Create: `src/layout.rs`** — the tiling tree. Pure data + math; no egui interaction, no `Session`. Everything unit-testable.
- **Modify: `src/wm.rs`** — swap rect source (zones → tree), drag/drop, resize, zoom, keyboard ops, deletions, test rewrites.
- **Modify: `src/keymap.rs`** — `TermFloat`/`ProjFloat` commands, relabel `TermSnap`/`ProjSnap` as "Move …" (serialization names unchanged — user keybinding files keep working).
- **Modify: `src/main.rs`** — register `mod layout;`, tile the startup project if one is spawned there.
- **Docs:** `docs/tiling-tree.md` (new), `docs/HANDOFF.md`, `docs/epics/window-tabbing-split-epic.md`, `docs/foreman.md`.

---

### Task 1: `src/layout.rs` — tree core (types, insert, remove)

**Files:**
- Create: `src/layout.rs`
- Modify: `src/main.rs` (add `mod layout;` next to the other `mod` lines)

- [ ] **Step 1: Confirm `WinId` and `Dir` are importable.** In `src/wm.rs`, `WinId` is a type alias and `Dir` a pub(crate) enum. If either is private, widen to `pub(crate)`. `Dir::zone()` will be deleted later; don't touch it now.

- [ ] **Step 2: Create `src/layout.rs` with types + failing-test scaffold**

```rust
//! The tiling layout tree. Pure data + math — no egui interaction, no Session.
//! A `WindowManager` owns one `LayoutTree`; windows whose ids appear as leaves
//! are "tiled" and get their rects from `layout()` each frame. Windows absent
//! from the tree are floating.

use crate::wm::{Dir, WinId};

/// No tile may shrink below this fraction of its split.
pub const MIN_RATIO: f32 = 0.10;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SplitDir {
    H, // children left→right
    V, // children top→bottom
}

impl SplitDir {
    fn of(d: Dir) -> SplitDir {
        match d {
            Dir::Left | Dir::Right => SplitDir::H,
            Dir::Up | Dir::Down => SplitDir::V,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Node {
    Leaf(WinId),
    Split {
        dir: SplitDir,
        ratios: Vec<f32>,   // same length as children, sums to 1.0
        children: Vec<Node>,
    },
}

#[derive(Clone, Debug, Default)]
pub struct LayoutTree {
    pub root: Option<Node>,
}

/// What dropping / inserting at a point would do. Returned with a hint rect to paint.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum DropTarget {
    /// Split the whole root on this side (or become the first tile of an empty tree).
    Root(Dir),
    /// Split this leaf on the given side.
    Split(WinId, Dir),
    /// Merge as a tab onto this leaf's window.
    Tab(WinId),
}
```

- [ ] **Step 3: Write failing tests for contains/leaves/insert/remove** (bottom of `src/layout.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_tree_has_no_leaves_and_contains_nothing() {
        let t = LayoutTree::default();
        assert!(t.is_empty());
        assert!(t.leaves().is_empty());
        assert!(!t.contains(1));
    }

    #[test]
    fn insert_root_on_empty_makes_a_sole_leaf() {
        let mut t = LayoutTree::default();
        t.insert_root(1, Dir::Right);
        assert!(t.contains(1));
        assert_eq!(t.leaves(), vec![1]);
    }

    #[test]
    fn insert_root_splits_an_existing_root() {
        let mut t = LayoutTree::default();
        t.insert_root(1, Dir::Right);
        t.insert_root(2, Dir::Left); // 2 takes the LEFT side
        assert_eq!(t.leaves(), vec![2, 1]); // left-to-right order
        t.insert_root(3, Dir::Down); // V split wrapping the H split
        assert_eq!(t.leaves(), vec![2, 1, 3]);
    }

    #[test]
    fn insert_split_replaces_a_leaf_with_a_split() {
        let mut t = LayoutTree::default();
        t.insert_root(1, Dir::Right);
        assert!(t.insert_split(1, 2, Dir::Down)); // 1 on top, 2 below
        assert_eq!(t.leaves(), vec![1, 2]);
        assert!(!t.insert_split(99, 3, Dir::Down)); // unknown target
    }

    #[test]
    fn insert_split_same_axis_becomes_a_flat_sibling() {
        let mut t = LayoutTree::default();
        t.insert_root(1, Dir::Right);
        t.insert_split(1, 2, Dir::Right); // H split: [1, 2]
        t.insert_split(2, 3, Dir::Right); // same axis: [1, 2, 3], NOT nested
        match t.root.as_ref().unwrap() {
            Node::Split { dir, ratios, children } => {
                assert_eq!(*dir, SplitDir::H);
                assert_eq!(children.len(), 3);
                assert!((ratios[0] - 0.5).abs() < 1e-4);
                assert!((ratios[1] - 0.25).abs() < 1e-4); // 2's 0.5 halved
                assert!((ratios[2] - 0.25).abs() < 1e-4);
            }
            _ => panic!("expected a flat 3-way split"),
        }
    }

    #[test]
    fn remove_collapses_single_child_splits_and_renormalizes() {
        let mut t = LayoutTree::default();
        t.insert_root(1, Dir::Right);
        t.insert_split(1, 2, Dir::Right);
        t.insert_split(2, 3, Dir::Down); // right pane is a V split [2, 3]
        assert!(t.remove(2));
        assert_eq!(t.leaves(), vec![1, 3]); // V split collapsed into leaf 3
        assert!(t.remove(3));
        assert_eq!(t.leaves(), vec![1]); // root collapsed to sole leaf
        assert!(t.remove(1));
        assert!(t.is_empty());
        assert!(!t.remove(1)); // already gone
    }

    #[test]
    fn remove_splices_same_dir_child_into_parent() {
        // H[1, V[2, 3]] — removing 1 collapses root to V[2, 3];
        // H[1, H-nested] can't be built by insert (flat siblings), so force it:
        let mut t = LayoutTree {
            root: Some(Node::Split {
                dir: SplitDir::H,
                ratios: vec![0.5, 0.5],
                children: vec![
                    Node::Leaf(1),
                    Node::Split {
                        dir: SplitDir::H,
                        ratios: vec![0.5, 0.5],
                        children: vec![Node::Leaf(2), Node::Leaf(3)],
                    },
                ],
            }),
        };
        t.remove(99); // no-op removal still triggers the flatten pass? No — only
                      // structural changes do. Remove a real leaf instead:
        t.remove(2);
        // nested single-child H collapsed to Leaf(3); tree is H[1, 3]
        match t.root.as_ref().unwrap() {
            Node::Split { children, .. } => assert_eq!(children.len(), 2),
            _ => panic!(),
        }
        assert_eq!(t.leaves(), vec![1, 3]);
    }
}
```

- [ ] **Step 4: Run tests, verify they fail to compile** (methods missing): `cargo test --bin foreman layout 2>&1 | Select-Object -Last 10`

- [ ] **Step 5: Implement the core ops**

```rust
impl LayoutTree {
    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    pub fn contains(&self, id: WinId) -> bool {
        fn walk(n: &Node, id: WinId) -> bool {
            match n {
                Node::Leaf(w) => *w == id,
                Node::Split { children, .. } => children.iter().any(|c| walk(c, id)),
            }
        }
        self.root.as_ref().is_some_and(|r| walk(r, id))
    }

    /// All leaf ids in visual order (left→right, top→bottom recursion order).
    pub fn leaves(&self) -> Vec<WinId> {
        fn walk(n: &Node, out: &mut Vec<WinId>) {
            match n {
                Node::Leaf(w) => out.push(*w),
                Node::Split { children, .. } => children.iter().for_each(|c| walk(c, out)),
            }
        }
        let mut out = Vec::new();
        if let Some(r) = &self.root {
            walk(r, &mut out);
        }
        out
    }

    /// Split the whole root: the new leaf takes the `side` half of the area.
    /// On an empty tree the new leaf simply becomes the root.
    pub fn insert_root(&mut self, id: WinId, side: Dir) {
        let new_leaf = Node::Leaf(id);
        self.root = Some(match self.root.take() {
            None => new_leaf,
            Some(old) => {
                let children = match side {
                    Dir::Left | Dir::Up => vec![new_leaf, old],
                    Dir::Right | Dir::Down => vec![old, new_leaf],
                };
                Node::Split { dir: SplitDir::of(side), ratios: vec![0.5, 0.5], children }
            }
        });
    }

    /// Split leaf `target` so the new leaf takes the `side` half of its slot.
    /// If the target's parent split already runs on that axis, the new leaf is
    /// inserted as a flat sibling (i3 behavior — keeps trees shallow); the
    /// target's ratio is halved between the two. Returns false if `target`
    /// isn't in the tree. On an empty tree the new leaf becomes the root.
    pub fn insert_split(&mut self, target: WinId, id: WinId, side: Dir) -> bool {
        fn go(n: &mut Node, target: WinId, id: WinId, side: Dir) -> bool {
            if let Node::Split { dir, ratios, children } = n {
                if *dir == SplitDir::of(side) {
                    if let Some(idx) = children
                        .iter()
                        .position(|c| matches!(c, Node::Leaf(w) if *w == target))
                    {
                        let half = ratios[idx] / 2.0;
                        ratios[idx] = half;
                        let at = match side {
                            Dir::Left | Dir::Up => idx,
                            Dir::Right | Dir::Down => idx + 1,
                        };
                        ratios.insert(at, half);
                        children.insert(at, Node::Leaf(id));
                        return true;
                    }
                }
                return children.iter_mut().any(|c| go(c, target, id, side));
            }
            if matches!(n, Node::Leaf(w) if *w == target) {
                let old = std::mem::replace(n, Node::Leaf(target)); // placeholder, overwritten below
                let new_leaf = Node::Leaf(id);
                let children = match side {
                    Dir::Left | Dir::Up => vec![new_leaf, old],
                    Dir::Right | Dir::Down => vec![old, new_leaf],
                };
                *n = Node::Split { dir: SplitDir::of(side), ratios: vec![0.5, 0.5], children };
                return true;
            }
            false
        }
        match &mut self.root {
            Some(r) => go(r, target, id, side),
            None => {
                self.root = Some(Node::Leaf(id));
                true
            }
        }
    }

    /// Remove a leaf. Siblings absorb its share (renormalized); single-child
    /// splits collapse; a child split running the same axis as its parent is
    /// spliced flat. Returns false if `id` wasn't in the tree.
    pub fn remove(&mut self, id: WinId) -> bool {
        fn prune(n: Node, id: WinId, removed: &mut bool) -> Option<Node> {
            match n {
                Node::Leaf(w) if w == id => {
                    *removed = true;
                    None
                }
                Node::Leaf(w) => Some(Node::Leaf(w)),
                Node::Split { dir, ratios, children } => {
                    let mut kept_c: Vec<Node> = Vec::new();
                    let mut kept_r: Vec<f32> = Vec::new();
                    for (c, r) in children.into_iter().zip(ratios) {
                        if let Some(c) = prune(c, id, removed) {
                            kept_c.push(c);
                            kept_r.push(r);
                        }
                    }
                    match kept_c.len() {
                        0 => None,
                        1 => Some(kept_c.pop().unwrap()),
                        _ => {
                            // splice same-axis child splits flat into this one
                            let mut flat_c: Vec<Node> = Vec::new();
                            let mut flat_r: Vec<f32> = Vec::new();
                            for (c, r) in kept_c.into_iter().zip(kept_r) {
                                match c {
                                    Node::Split { dir: cd, ratios: cr, children: cc }
                                        if cd == dir =>
                                    {
                                        for (gc, gr) in cc.into_iter().zip(cr) {
                                            flat_c.push(gc);
                                            flat_r.push(gr * r);
                                        }
                                    }
                                    other => {
                                        flat_c.push(other);
                                        flat_r.push(r);
                                    }
                                }
                            }
                            let total: f32 = flat_r.iter().sum();
                            for r in &mut flat_r {
                                *r /= total;
                            }
                            Some(Node::Split { dir, ratios: flat_r, children: flat_c })
                        }
                    }
                }
            }
        }
        let mut removed = false;
        self.root = self.root.take().and_then(|r| prune(r, id, &mut removed));
        removed
    }
}
```

- [ ] **Step 6: Add `mod layout;` to `src/main.rs`** next to the existing module declarations (`mod wm;` etc.).

- [ ] **Step 7: Run tests, verify pass**: `cargo test --bin foreman layout 2>&1 | Select-Object -Last 10` → all green. Fix the `remove_splices…` test if its forced-tree comment confuses the borrow checker — the assertions are the contract.

- [ ] **Step 8: Commit**: `git add src/layout.rs src/main.rs` ; `git commit -m "feat(layout): tiling tree core — insert, remove, flatten"`

---

### Task 2: `layout()` — rect computation

**Files:** Modify: `src/layout.rs`

- [ ] **Step 1: Write failing tests**

```rust
    fn area() -> egui::Rect {
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 800.0))
    }

    #[test]
    fn layout_single_leaf_fills_area_minus_outer_gap() {
        let mut t = LayoutTree::default();
        t.insert_root(1, Dir::Right);
        let p = t.layout(area(), 8.0);
        assert_eq!(p.len(), 1);
        let r = p[0].1;
        assert!((r.min.x - 8.0).abs() < 0.01 && (r.max.x - 992.0).abs() < 0.01);
        assert!((r.min.y - 8.0).abs() < 0.01 && (r.max.y - 792.0).abs() < 0.01);
    }

    #[test]
    fn layout_h_split_divides_width_by_ratio_with_gap() {
        let mut t = LayoutTree::default();
        t.insert_root(1, Dir::Right);
        t.insert_split(1, 2, Dir::Right);
        let p = t.layout(area(), 8.0);
        // inner width 984, one gap 8 → 976 shared 50/50 = 488 each
        let r1 = p.iter().find(|(w, _)| *w == 1).unwrap().1;
        let r2 = p.iter().find(|(w, _)| *w == 2).unwrap().1;
        assert!((r1.width() - 488.0).abs() < 0.01);
        assert!((r2.width() - 488.0).abs() < 0.01);
        assert!((r2.min.x - (r1.max.x + 8.0)).abs() < 0.01);
        assert!((r1.height() - 784.0).abs() < 0.01); // full inner height
    }

    #[test]
    fn layout_nested_v_inside_h() {
        let mut t = LayoutTree::default();
        t.insert_root(1, Dir::Right);
        t.insert_split(1, 2, Dir::Right);
        t.insert_split(2, 3, Dir::Down);
        let p = t.layout(area(), 8.0);
        let r2 = p.iter().find(|(w, _)| *w == 2).unwrap().1;
        let r3 = p.iter().find(|(w, _)| *w == 3).unwrap().1;
        assert!((r2.min.x - r3.min.x).abs() < 0.01); // stacked in the same column
        assert!(r3.min.y > r2.max.y); // 3 below 2
    }
```

- [ ] **Step 2: Run, verify fail** (no `layout` method).

- [ ] **Step 3: Implement**

```rust
    /// Compute leaf rects within `area` (any coordinate space — local or screen),
    /// with `gap` pixels between siblings and as the outer margin (matches the
    /// old `zone_rect` SNAP_GAP geometry).
    pub fn layout(&self, area: egui::Rect, gap: f32) -> Vec<(WinId, egui::Rect)> {
        fn walk(n: &Node, r: egui::Rect, gap: f32, out: &mut Vec<(WinId, egui::Rect)>) {
            match n {
                Node::Leaf(w) => out.push((*w, r)),
                Node::Split { dir, ratios, children } => {
                    let gaps = gap * (children.len() - 1) as f32;
                    match dir {
                        SplitDir::H => {
                            let avail = (r.width() - gaps).max(1.0);
                            let mut x = r.min.x;
                            for (c, ratio) in children.iter().zip(ratios) {
                                let w = avail * ratio;
                                let cr = egui::Rect::from_min_size(
                                    egui::pos2(x, r.min.y),
                                    egui::vec2(w, r.height()),
                                );
                                walk(c, cr, gap, out);
                                x += w + gap;
                            }
                        }
                        SplitDir::V => {
                            let avail = (r.height() - gaps).max(1.0);
                            let mut y = r.min.y;
                            for (c, ratio) in children.iter().zip(ratios) {
                                let h = avail * ratio;
                                let cr = egui::Rect::from_min_size(
                                    egui::pos2(r.min.x, y),
                                    egui::vec2(r.width(), h),
                                );
                                walk(c, cr, gap, out);
                                y += h + gap;
                            }
                        }
                    }
                }
            }
        }
        let mut out = Vec::new();
        if let Some(r) = &self.root {
            walk(r, area.shrink(gap), gap, &mut out);
        }
        out
    }
```

- [ ] **Step 4: Run tests, verify pass.**
- [ ] **Step 5: Commit**: `git commit -am "feat(layout): rect computation with gaps and ratios"`

---

### Task 3: `hit_leaf` + `drop_target`

**Files:** Modify: `src/layout.rs`

- [ ] **Step 1: Write failing tests**

```rust
    #[test]
    fn hit_leaf_finds_the_leaf_under_a_point() {
        let mut t = LayoutTree::default();
        t.insert_root(1, Dir::Right);
        t.insert_split(1, 2, Dir::Right);
        let (id, _) = t.hit_leaf(egui::pos2(100.0, 400.0), area(), 8.0).unwrap();
        assert_eq!(id, 1);
        let (id, _) = t.hit_leaf(egui::pos2(900.0, 400.0), area(), 8.0).unwrap();
        assert_eq!(id, 2);
        assert!(t.hit_leaf(egui::pos2(-50.0, 400.0), area(), 8.0).is_none());
    }

    #[test]
    fn drop_target_center_tabs_edges_split() {
        let mut t = LayoutTree::default();
        t.insert_root(1, Dir::Right);
        // dead center of the sole leaf → Tab
        let (tgt, _) = t.drop_target(egui::pos2(500.0, 400.0), area(), 8.0).unwrap();
        assert_eq!(tgt, DropTarget::Tab(1));
        // inside the leaf, far left (but outside the 8.5% area edge band) → Split left
        let (tgt, _) = t.drop_target(egui::pos2(200.0, 400.0), area(), 8.0).unwrap();
        assert_eq!(tgt, DropTarget::Split(1, Dir::Left));
        // near the bottom of the leaf → Split down
        let (tgt, _) = t.drop_target(egui::pos2(500.0, 700.0), area(), 8.0).unwrap();
        assert_eq!(tgt, DropTarget::Split(1, Dir::Down));
    }

    #[test]
    fn drop_target_area_edge_band_splits_the_root() {
        let mut t = LayoutTree::default();
        t.insert_root(1, Dir::Right);
        let (tgt, _) = t.drop_target(egui::pos2(10.0, 400.0), area(), 8.0).unwrap();
        assert_eq!(tgt, DropTarget::Root(Dir::Left));
        let (tgt, _) = t.drop_target(egui::pos2(500.0, 795.0), area(), 8.0).unwrap();
        assert_eq!(tgt, DropTarget::Root(Dir::Down));
    }

    #[test]
    fn drop_target_on_empty_tree_uses_edge_band_only() {
        let t = LayoutTree::default();
        assert!(t.drop_target(egui::pos2(500.0, 400.0), area(), 8.0).is_none()); // center: nothing
        let (tgt, hint) = t.drop_target(egui::pos2(10.0, 400.0), area(), 8.0).unwrap();
        assert!(matches!(tgt, DropTarget::Root(_)));
        assert!((hint.width() - 984.0).abs() < 0.01); // full inner area
    }
```

- [ ] **Step 2: Run, verify fail.**

- [ ] **Step 3: Implement**

```rust
    /// The leaf whose rect (expanded to cover half the gap) contains `p`.
    pub fn hit_leaf(&self, p: egui::Pos2, area: egui::Rect, gap: f32) -> Option<(WinId, egui::Rect)> {
        self.layout(area, gap)
            .into_iter()
            .find(|(_, r)| r.expand(gap * 0.5 + 1.0).contains(p))
    }

    /// What inserting a window at `p` would do, plus the hint rect to paint.
    /// Precedence: area edge band (root split) → leaf center (tab) → leaf
    /// nearest-edge (split). Empty tree: edge band makes the first tile.
    pub fn drop_target(&self, p: egui::Pos2, area: egui::Rect, gap: f32) -> Option<(DropTarget, egui::Rect)> {
        const EDGE: f32 = 0.085; // same band feel as the old detect_zone
        let fx = (p.x - area.min.x) / area.width();
        let fy = (p.y - area.min.y) / area.height();
        if !(0.0..=1.0).contains(&fx) || !(0.0..=1.0).contains(&fy) {
            return None;
        }
        let inner = area.shrink(gap);
        if self.root.is_none() {
            let on_edge = fx < EDGE || fx > 1.0 - EDGE || fy < EDGE || fy > 1.0 - EDGE;
            return on_edge.then_some((DropTarget::Root(Dir::Right), inner));
        }
        let half = |side: Dir| -> egui::Rect {
            match side {
                Dir::Left => egui::Rect::from_min_max(inner.min, egui::pos2(inner.center().x, inner.max.y)),
                Dir::Right => egui::Rect::from_min_max(egui::pos2(inner.center().x, inner.min.y), inner.max),
                Dir::Up => egui::Rect::from_min_max(inner.min, egui::pos2(inner.max.x, inner.center().y)),
                Dir::Down => egui::Rect::from_min_max(egui::pos2(inner.min.x, inner.center().y), inner.max),
            }
        };
        if fx < EDGE {
            return Some((DropTarget::Root(Dir::Left), half(Dir::Left)));
        }
        if fx > 1.0 - EDGE {
            return Some((DropTarget::Root(Dir::Right), half(Dir::Right)));
        }
        if fy < EDGE {
            return Some((DropTarget::Root(Dir::Up), half(Dir::Up)));
        }
        if fy > 1.0 - EDGE {
            return Some((DropTarget::Root(Dir::Down), half(Dir::Down)));
        }
        let (id, r) = self.hit_leaf(p, area, gap)?;
        let cx = ((p.x - r.min.x) / r.width()).clamp(0.0, 1.0);
        let cy = ((p.y - r.min.y) / r.height()).clamp(0.0, 1.0);
        if (0.30..=0.70).contains(&cx) && (0.30..=0.70).contains(&cy) {
            return Some((DropTarget::Tab(id), r));
        }
        let (dl, dr, dt, db) = (cx, 1.0 - cx, cy, 1.0 - cy);
        let side = if dl <= dr && dl <= dt && dl <= db {
            Dir::Left
        } else if dr <= dt && dr <= db {
            Dir::Right
        } else if dt <= db {
            Dir::Up
        } else {
            Dir::Down
        };
        let hint = match side {
            Dir::Left => egui::Rect::from_min_max(r.min, egui::pos2(r.center().x, r.max.y)),
            Dir::Right => egui::Rect::from_min_max(egui::pos2(r.center().x, r.min.y), r.max),
            Dir::Up => egui::Rect::from_min_max(r.min, egui::pos2(r.max.x, r.center().y)),
            Dir::Down => egui::Rect::from_min_max(egui::pos2(r.min.x, r.center().y), r.max),
        };
        Some((DropTarget::Split(id, side), hint))
    }
```

- [ ] **Step 4: Run tests, verify pass.**
- [ ] **Step 5: Commit**: `git commit -am "feat(layout): hit-testing and drop targets for drag insertion"`

---

### Task 4: `swap` + `resize_edge`

**Files:** Modify: `src/layout.rs`

- [ ] **Step 1: Write failing tests**

```rust
    #[test]
    fn swap_exchanges_two_leaves() {
        let mut t = LayoutTree::default();
        t.insert_root(1, Dir::Right);
        t.insert_split(1, 2, Dir::Right);
        assert!(t.swap(1, 2));
        assert_eq!(t.leaves(), vec![2, 1]);
        assert!(!t.swap(1, 99));
    }

    #[test]
    fn resize_edge_moves_the_shared_divider() {
        let mut t = LayoutTree::default();
        t.insert_root(1, Dir::Right);
        t.insert_split(1, 2, Dir::Right);
        // drag 1's RIGHT edge +97.6px → +0.1 ratio (avail width = 976)
        assert!(t.resize_edge(1, Dir::Right, 97.6, area(), 8.0));
        let p = t.layout(area(), 8.0);
        let r1 = p.iter().find(|(w, _)| *w == 1).unwrap().1;
        assert!((r1.width() - 585.6).abs() < 0.5); // 976 * 0.6
        // dragging 2's LEFT edge by the same delta moves the same divider
        assert!(t.resize_edge(2, Dir::Left, -97.6, area(), 8.0));
        let p = t.layout(area(), 8.0);
        let r1 = p.iter().find(|(w, _)| *w == 1).unwrap().1;
        assert!((r1.width() - 488.0).abs() < 0.5); // back to 50/50
    }

    #[test]
    fn resize_outer_edge_is_a_noop_and_min_ratio_clamps() {
        let mut t = LayoutTree::default();
        t.insert_root(1, Dir::Right);
        t.insert_split(1, 2, Dir::Right);
        assert!(!t.resize_edge(1, Dir::Left, 50.0, area(), 8.0)); // 1 owns no left divider
        assert!(!t.resize_edge(1, Dir::Up, 50.0, area(), 8.0));   // no vertical split at all
        t.resize_edge(1, Dir::Right, 100_000.0, area(), 8.0);     // absurd drag
        let p = t.layout(area(), 8.0);
        let r2 = p.iter().find(|(w, _)| *w == 2).unwrap().1;
        assert!(r2.width() >= 976.0 * MIN_RATIO - 0.5); // clamped, not crushed
    }
```

- [ ] **Step 2: Run, verify fail.**

- [ ] **Step 3: Implement**

```rust
    /// Swap the positions of two leaves. False unless both are present.
    pub fn swap(&mut self, a: WinId, b: WinId) -> bool {
        if a == b || !self.contains(a) || !self.contains(b) {
            return false;
        }
        fn walk(n: &mut Node, a: WinId, b: WinId) {
            match n {
                Node::Leaf(w) => {
                    if *w == a {
                        *w = b;
                    } else if *w == b {
                        *w = a;
                    }
                }
                Node::Split { children, .. } => children.iter_mut().for_each(|c| walk(c, a, b)),
            }
        }
        if let Some(r) = &mut self.root {
            walk(r, a, b);
        }
        true
    }

    /// Drag the divider on `edge` of leaf `id` by `delta_px`. Resolves to the
    /// deepest ancestor split running on that axis where the edge is interior;
    /// outer edges (no such divider) return false. Both affected ratios are
    /// clamped to MIN_RATIO.
    pub fn resize_edge(
        &mut self,
        id: WinId,
        edge: Dir,
        delta_px: f32,
        area: egui::Rect,
        gap: f32,
    ) -> bool {
        let axis = SplitDir::of(edge);
        // Pass 1 (read-only): find (address-of-split, child-index, avail-extent)
        // of the deepest matching split along the path to `id`.
        fn find(
            n: &Node,
            r: egui::Rect,
            id: WinId,
            edge: Dir,
            axis: SplitDir,
            gap: f32,
            addr: Vec<usize>,
        ) -> Option<(Vec<usize>, usize, f32)> {
            let Node::Split { dir, ratios, children } = n else {
                return None;
            };
            // which child subtree holds `id`?
            fn holds(n: &Node, id: WinId) -> bool {
                match n {
                    Node::Leaf(w) => *w == id,
                    Node::Split { children, .. } => children.iter().any(|c| holds(c, id)),
                }
            }
            let idx = children.iter().position(|c| holds(c, id))?;
            // child rect, same math as layout()
            let gaps = gap * (children.len() - 1) as f32;
            let avail = match dir {
                SplitDir::H => (r.width() - gaps).max(1.0),
                SplitDir::V => (r.height() - gaps).max(1.0),
            };
            let lead: f32 = ratios[..idx].iter().sum::<f32>() * avail + gap * idx as f32;
            let extent = ratios[idx] * avail;
            let child_rect = match dir {
                SplitDir::H => egui::Rect::from_min_size(
                    egui::pos2(r.min.x + lead, r.min.y),
                    egui::vec2(extent, r.height()),
                ),
                SplitDir::V => egui::Rect::from_min_size(
                    egui::pos2(r.min.x, r.min.y + lead),
                    egui::vec2(r.width(), extent),
                ),
            };
            let mut child_addr = addr.clone();
            child_addr.push(idx);
            let deeper = find(&children[idx], child_rect, id, edge, axis, gap, child_addr);
            if deeper.is_some() {
                return deeper;
            }
            // this split is the owner if axis matches and the edge is interior here
            let interior = *dir == axis
                && match edge {
                    Dir::Left | Dir::Up => idx > 0,
                    Dir::Right | Dir::Down => idx < children.len() - 1,
                };
            interior.then_some((addr, idx, avail))
        }
        let root = self.root.as_ref()?;
        // (a `?` needs Option; wrap in a closure-free match instead:)
        let found = match &self.root {
            Some(r) => find(r, area.shrink(gap), id, edge, axis, gap, Vec::new()),
            None => None,
        };
        let Some((addr, idx, avail)) = found else {
            return false;
        };
        let _ = root; // silence unused if the compiler complains; remove otherwise
        // Pass 2: descend by address and adjust the two ratios.
        let mut node = self.root.as_mut().unwrap();
        for i in addr {
            let Node::Split { children, .. } = node else { unreachable!() };
            node = &mut children[i];
        }
        let Node::Split { ratios, .. } = node else { unreachable!() };
        let (a, b) = match edge {
            Dir::Left | Dir::Up => (idx - 1, idx),
            Dir::Right | Dir::Down => (idx, idx + 1),
        };
        let df = (delta_px / avail).clamp(-(ratios[a] - MIN_RATIO), ratios[b] - MIN_RATIO);
        ratios[a] += df;
        ratios[b] -= df;
        true
    }
```

Note: `resize_edge` returns `bool`, so the early `let root = self.root.as_ref()?` line above is wrong as written — delete it and rely on the `match &self.root` that follows (the snippet keeps both to show the intent; final code has only the `match`).

- [ ] **Step 4: Run tests, verify pass.** `cargo test --bin foreman layout 2>&1 | Select-Object -Last 10`
- [ ] **Step 5: Commit**: `git commit -am "feat(layout): leaf swap and ratio-based divider resize"`

---

### Task 5: wm.rs — plumb `tree` + `zoomed`, tree-first refit (no behavior change yet)

**Files:** Modify: `src/wm.rs` (struct `WindowManager`, fn `new`, fn `show`)

Nothing inserts into the tree yet, so behavior is identical; this lands the rails.

- [ ] **Step 1: Add fields.** In `WindowManager`, after `split: egui::Vec2`, add:

```rust
    /// The tiling tree: windows whose ids are leaves are *tiled* and take their
    /// rect from `tree.layout()` each frame. Everything else floats.
    tree: crate::layout::LayoutTree,
    /// tmux-style zoom: render this window full-area on top, tree untouched.
    zoomed: Option<WinId>,
```

Initialize in `WindowManager::new` (find the struct literal; add `tree: Default::default(), zoomed: None,`).

- [ ] **Step 2: Add the `detach` helper** (near `close_tab`):

```rust
    /// Pull `id` out of the tiled layer entirely: drop its tree leaf (siblings
    /// absorb the space) and clear zoom if it was the zoomed window. Safe no-op
    /// for floating windows. Call before any close/minimize/merge-consume/tear-out.
    fn detach(&mut self, id: WinId) {
        self.tree.remove(id);
        if self.zoomed == Some(id) {
            self.zoomed = None;
        }
    }
```

- [ ] **Step 3: Refit from the tree first.** In `show`, after `order.sort_by_key(...)`, insert:

```rust
        let placements: std::collections::HashMap<WinId, egui::Rect> = self
            .tree
            .layout(egui::Rect::from_min_size(egui::Pos2::ZERO, asz), SNAP_GAP)
            .into_iter()
            .collect();
        // zoomed window renders last (on top of the tiles)
        if let Some(zid) = self.zoomed {
            if let Some(pos) = order.iter().position(|&i| self.windows[i].id == zid) {
                let v = order.remove(pos);
                order.push(v);
            }
        }
```

Replace the per-window refit block

```rust
            {
                let w = &mut self.windows[i];
                match w.snap {
                    Some(z) => w.rect = zone_rect(z, asz, self.split),
                    None => clamp(&mut w.rect, asz),
                }
            }
```

with

```rust
            let is_tiled = placements.contains_key(&id);
            {
                let zoomed = self.zoomed;
                let w = &mut self.windows[i];
                if zoomed == Some(w.id) {
                    w.rect = egui::Rect::from_min_size(egui::Pos2::ZERO, asz).shrink(SNAP_GAP);
                } else if let Some(r) = placements.get(&w.id) {
                    w.rect = *r;
                } else {
                    match w.snap {
                        Some(z) => w.rect = zone_rect(z, asz, self.split),
                        None => clamp(&mut w.rect, asz),
                    }
                }
            }
```

- [ ] **Step 4: Square corners for tiled/zoomed too.** Replace the `cr` computation (`if self.windows[i].snap.is_some()`) with `if is_tiled || self.zoomed == Some(id) || self.windows[i].snap.is_some()`.

- [ ] **Step 5: Hook `detach` into lifecycle paths.** Using Serena, add `self.detach(id);` (or `self.detach(src);`) as the FIRST line of: `close` (whole-window close), the `Act::Min(id)` arm in `apply_acts`, and in `merge_windows` immediately before `let src_win = self.windows.remove(si);`.

- [ ] **Step 6: Build + full test run** — `cargo test --bin foreman 2>&1 | Select-Object -Last 10` → green (behavior unchanged; tree is empty at runtime).

- [ ] **Step 7: Commit**: `git commit -am "feat(wm): layout tree + zoom fields plumbed into the render loop"`

---

### Task 6: Zoom → `zoomed` overlay (drop `Zone::Max` semantics)

**Files:** Modify: `src/wm.rs` (`toggle_zoom`, `apply_acts` `Act::Max` arm, `dispatch` zoom arms)

- [ ] **Step 1: Write a failing test** in the wm `tests` mod (read the mod's existing helpers — `stub_content`, `push` — first and match their shape):

```rust
    #[test]
    fn zoom_overlays_without_touching_the_tree_or_floating_rect() {
        let mut wm = WindowManager::new(...); // mirror neighbouring tests' construction
        let a = push(&mut wm, "a"); // stub window helpers from this mod
        wm.tree.insert_root(a, Dir::Right);
        wm.toggle_zoom(a);
        assert_eq!(wm.zoomed, Some(a));
        assert!(wm.tree.contains(a)); // tree untouched
        wm.toggle_zoom(a);
        assert_eq!(wm.zoomed, None);
        // floating window: rect must survive a zoom round-trip
        let b = push(&mut wm, "b");
        let before = wm.windows.iter().find(|w| w.id == b).unwrap().rect;
        wm.toggle_zoom(b);
        wm.toggle_zoom(b);
        let after = wm.windows.iter().find(|w| w.id == b).unwrap().rect;
        assert_eq!(before, after);
    }
```

- [ ] **Step 2: Run, verify fail** (signature mismatch / old semantics).

- [ ] **Step 3: Replace `toggle_zoom`** (new signature — no `asz`):

```rust
    /// tmux-style zoom: render the window full-area on top. The tree and other
    /// windows are untouched; un-zoom restores instantly. A floating window's
    /// rect round-trips via `prev`.
    fn toggle_zoom(&mut self, id: WinId) {
        if self.zoomed == Some(id) {
            self.zoomed = None;
            if !self.tree.contains(id) {
                if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                    if let Some(pr) = w.prev.take() {
                        w.rect = pr;
                    }
                }
            }
        } else {
            if !self.tree.contains(id) {
                if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                    w.prev = Some(w.rect);
                }
            }
            self.zoomed = Some(id);
        }
        self.focus(id);
    }
```

- [ ] **Step 4: Reroute callers.** `apply_acts` `Act::Max(id)` arm becomes just `Act::Max(id) => self.toggle_zoom(id),`. In `dispatch`, `ZoomProject`/`ZoomTerm` arms drop the `asz` argument (`self.toggle_zoom(id)` / `child.toggle_zoom(id)`); delete the now-unused `asz_proj`/`asz` lets if the compiler flags them.

- [ ] **Step 5: Run full tests** — fix any existing zoom test expecting `Zone::Max` (rewrite its assertions to `wm.zoomed`).
- [ ] **Step 6: Commit**: `git commit -am "feat(wm): tmux-style zoom overlay replaces Zone::Max"`

---

### Task 7: keymap — `TermFloat` / `ProjFloat` commands

**Files:** Modify: `src/keymap.rs`

- [ ] **Step 1: Write failing test** (keymap tests mod):

```rust
    #[test]
    fn float_toggle_defaults_are_f_and_ctrl_f() {
        let km = Keymap::default();
        assert_eq!(km.resolve(Chord::new(egui::Key::F, false, false, false)), Some(Command::TermFloat));
        assert_eq!(km.resolve(Chord::new(egui::Key::F, true, false, false)), Some(Command::ProjFloat));
    }
```

- [ ] **Step 2: Run, verify fail** (no such variants).

- [ ] **Step 3: Implement.** Add `TermFloat` to the `Command` enum (after `LastTerm`) and `ProjFloat` (after `LastProject`). Then:
  - `Command::ALL`: insert `ProjFloat` after `LastProject`, `TermFloat` after `LastTerm`.
  - `group()`: `ProjFloat` → `Group::Projects`, `TermFloat` → `Group::Terminals`.
  - `label()`: `TermFloat => "Float / re-tile terminal"`, `ProjFloat => "Float / re-tile project"`.
  - `Keymap::default()`: `t.insert(plain(K::F), TermFloat); t.insert(ctrl(K::F), ProjFloat);` (F is unbound today — verified against the current default table).
  - `dispatch` in wm.rs has a `_ => {}` catch-all, so this compiles before Task 8 wires it.

- [ ] **Step 4: Run keymap tests** — the existing `all_commands_have_a_default_chord_and_metadata` test must also pass (it sweeps `ALL`).
- [ ] **Step 5: Commit**: `git commit -am "feat(keymap): TermFloat/ProjFloat commands on leader F / Ctrl+F"`

---

### Task 8: Keyboard switchover — `move_dir`, tree split, `toggle_float`

**Files:** Modify: `src/wm.rs` (`snap_dir`→`move_dir`, `split_dir`, `place_split`, new `toggle_float`, `dispatch`; tests)

Read the wm `tests` mod helpers first; the tests below must be adapted to their real signatures — the assertions are the contract.

- [ ] **Step 1: Write failing tests**

```rust
    #[test]
    fn move_dir_swaps_with_the_neighbor_and_edges_out() {
        // two tiles side by side; focus left; move right → swapped
        // then move right again (no neighbor) → re-inserted at the right edge (still 2 leaves)
    }

    #[test]
    fn move_dir_on_a_floating_window_enters_the_tree_at_that_edge() {
        // floating focused + move_dir(Left) → tree.contains(id), leaf order puts it left
    }

    #[test]
    fn split_from_tiled_source_splits_that_leaf() {
        // place_split(Some(src tiled), new, Dir::Right) → both tiled, new to src's right
    }

    #[test]
    fn split_from_floating_source_tiles_both_panes() {
        // src floating: place_split inserts src into the tree first, then splits →
        // tree.leaves() == [src, new] for Dir::Right
    }

    #[test]
    fn toggle_float_roundtrips_tree_membership_and_rect() {
        // tiled → float: not in tree, rect == prev; float → tile: back in tree
    }
```

Flesh each out with the mod's `push`/`stub_content` helpers exactly as the neighbouring `split_from_floating_source_snaps_both_panes` test does today (you are replacing that test and `split_from_snapped_source_leaves_source_untouched`, `split_into_occupied_zone_tabs_onto_occupant`, `snap_dir_composes_into_and_out_of_a_corner`, `snap_dir_into_occupied_corner_tabs_onto_occupant` — delete those five).

- [ ] **Step 2: Run, verify fail.**

- [ ] **Step 3: Replace `snap_dir` with `move_dir`** (and rename the two `dispatch` call sites `TermSnap`/`ProjSnap` → `self.move_dir(d)` / `child.move_dir(d)` — the *Command names stay* for keybinding-file compat):

```rust
    /// Move the focused window within the tiled layer. Tiled: swap with the
    /// geometric neighbor leaf in that direction; with no neighbor, re-insert at
    /// the area edge as a full row/column. Floating: enter the tree at that edge.
    fn move_dir(&mut self, d: Dir) {
        let Some(id) = self.focused else { return };
        if self.tree.contains(id) {
            let local = egui::Rect::from_min_size(egui::Pos2::ZERO, self.last_area);
            let placements = self.tree.layout(local, SNAP_GAP);
            let Some(from) = placements.iter().find(|(w, _)| *w == id).map(|(_, r)| r.center())
            else {
                return;
            };
            let mut best: Option<(WinId, f32)> = None;
            for (w, r) in placements.iter().filter(|(w, _)| *w != id) {
                let c = r.center();
                let (along, cross) = match d {
                    Dir::Left => (from.x - c.x, (c.y - from.y).abs()),
                    Dir::Right => (c.x - from.x, (c.y - from.y).abs()),
                    Dir::Up => (from.y - c.y, (c.x - from.x).abs()),
                    Dir::Down => (c.y - from.y, (c.x - from.x).abs()),
                };
                if along <= 1.0 {
                    continue;
                }
                let score = along + cross * 2.0;
                if best.is_none_or(|(_, b)| score < b) {
                    best = Some((*w, score));
                }
            }
            match best {
                Some((n, _)) => {
                    self.tree.swap(id, n);
                }
                None => {
                    self.tree.remove(id);
                    self.tree.insert_root(id, d);
                }
            }
        } else {
            if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                if w.prev.is_none() {
                    w.prev = Some(w.rect);
                }
            }
            self.tree.insert_root(id, d);
        }
        self.focus(id);
    }
```

- [ ] **Step 4: Rewrite `split_dir` / `place_split`**

```rust
    /// Split: create a new terminal next to the focused window in the tree.
    fn split_dir(&mut self, d: Dir, ctx: &egui::Context) {
        let src = self.focused;
        let Some(new_id) = self.add_terminal(Shell::PowerShell, ctx) else {
            return;
        };
        self.place_split(src, new_id, d);
    }

    /// The pure placement half of [`split_dir`] (no PTY/spawn), testable without
    /// a real `Session`. A floating (or absent) source first enters the tree so
    /// `Alt+WASD` always yields the two-pane result the user expects.
    fn place_split(&mut self, src: Option<WinId>, new_id: WinId, d: Dir) {
        let anchor = match src.filter(|s| *s != new_id) {
            Some(s) if self.tree.contains(s) => Some(s),
            Some(s) => {
                if let Some(w) = self.windows.iter_mut().find(|w| w.id == s) {
                    if w.prev.is_none() {
                        w.prev = Some(w.rect);
                    }
                }
                self.tree.insert_root(s, Dir::Right);
                Some(s)
            }
            None => None,
        };
        if let Some(w) = self.windows.iter_mut().find(|w| w.id == new_id) {
            if w.prev.is_none() {
                w.prev = Some(w.rect);
            }
        }
        match anchor {
            Some(a) => {
                self.tree.insert_split(a, new_id, d);
            }
            None => self.tree.insert_root(new_id, d),
        }
        self.focus(new_id);
    }
```

- [ ] **Step 5: Add `toggle_float` + dispatch wiring**

```rust
    /// Toggle the focused window between tiled and floating. Un-tiling restores
    /// the remembered floating rect; re-tiling enters the tree where the window
    /// currently sits (the leaf under its center, split along its longer axis).
    fn toggle_float(&mut self) {
        let Some(id) = self.focused else { return };
        if self.tree.contains(id) {
            self.detach(id);
            if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                w.rect = w.prev.take().unwrap_or(egui::Rect::from_min_size(
                    egui::pos2(60.0, 60.0),
                    egui::vec2(580.0, 380.0),
                ));
            }
        } else {
            let (center, rect) = match self.windows.iter().find(|w| w.id == id) {
                Some(w) => (w.rect.center(), w.rect),
                None => return,
            };
            if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                w.prev = Some(rect);
            }
            let local = egui::Rect::from_min_size(egui::Pos2::ZERO, self.last_area);
            match self.tree.hit_leaf(center, local, SNAP_GAP) {
                Some((leaf, r)) => {
                    let side = if r.width() >= r.height() { Dir::Right } else { Dir::Down };
                    self.tree.insert_split(leaf, id, side);
                }
                None => self.tree.insert_root(id, Dir::Right),
            }
        }
        self.focus(id);
    }
```

In `dispatch`: project block gets `Command::ProjFloat => self.toggle_float(),`; terminal block gets `Command::TermFloat => child.toggle_float(),`.

- [ ] **Step 6: Run full tests; fix fallout.** The five deleted zone tests are gone; everything else must be green.
- [ ] **Step 7: Commit**: `git commit -am "feat(wm): tree-based move/split/float keyboard commands"`

---

### Task 9: Mouse switchover — header tear-out, drop hints, drop commit

**Files:** Modify: `src/wm.rs` (`show` drag sections, `paint_drag_overlays` unchanged in signature)

This is interactive code — no unit tests; verified in Task 14. Keep edits surgical.

- [ ] **Step 1: Tear-out on drag.** In `show`, replace the body of `if dr.dragged() { ... }` up to (not including) the merge-target detection with:

```rust
            if dr.dragged() {
                let popped = self.tree.contains(id) || self.zoomed == Some(id);
                if popped {
                    self.detach(id);
                }
                {
                    let w = &mut self.windows[i];
                    if popped {
                        if let (Some(pr), Some(p)) = (w.prev.take(), ui.ctx().pointer_latest_pos())
                        {
                            let local = p - area.min.to_vec2();
                            let frac = if w.rect.width() > 0.0 {
                                ((local.x - w.rect.min.x) / w.rect.width()).clamp(0.0, 1.0)
                            } else {
                                0.5
                            };
                            w.rect = egui::Rect::from_min_size(
                                egui::pos2(local.x - frac * pr.width(), local.y - TITLE_H * 0.5),
                                pr.size(),
                            );
                        }
                    }
                    w.rect = w.rect.translate(dr.drag_delta());
                    clamp(&mut w.rect, asz);
                }
                scr = self.windows[i].rect.translate(area.min.to_vec2());
```

(Identical re-anchor math to the old snap-pop; only the state source changed.)

- [ ] **Step 2: Hints.** Replace the old `else` branch (the `detect_zone`/dwell/`resolve_zone` block) of the merge-target check with:

```rust
                let pointer = ui.ctx().pointer_latest_pos();
                let over_target = pointer.and_then(|p| self.merge_target_at(id, p, area, &order));
                if let Some(tgt) = over_target {
                    merge_hint = Some(tgt);
                } else if let Some(p) = pointer {
                    if let Some((_, hint)) = self.tree.drop_target(p, area, SNAP_GAP) {
                        snap_overlay = Some(hint);
                    }
                }
            }
```

(`drop_target` is coordinate-space agnostic: passing the screen `area` yields screen hint rects, which is what `paint_drag_overlays` paints.)

- [ ] **Step 3: Drop commit.** Replace the whole `if dr.drag_stopped() { ... }` block with:

```rust
            if dr.drag_stopped() {
                let pointer = ui.ctx().pointer_latest_pos();
                let merge_dst = pointer.and_then(|p| self.merge_target_at(id, p, area, &order));
                if let Some(dst_i) = merge_dst {
                    let dst = self.windows[dst_i].id;
                    acts.push(Act::Merge { src: id, dst });
                } else if let Some(p) = pointer {
                    if let Some((target, _)) = self.tree.drop_target(p, area, SNAP_GAP) {
                        match target {
                            crate::layout::DropTarget::Tab(dst) => {
                                acts.push(Act::Merge { src: id, dst });
                            }
                            crate::layout::DropTarget::Split(t, side) => {
                                if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                                    if w.prev.is_none() {
                                        w.prev = Some(w.rect);
                                    }
                                }
                                self.tree.insert_split(t, id, side);
                            }
                            crate::layout::DropTarget::Root(side) => {
                                if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                                    if w.prev.is_none() {
                                        w.prev = Some(w.rect);
                                    }
                                }
                                self.tree.insert_root(id, side);
                            }
                        }
                    }
                }
            }
```

The rect refits from `placements` next frame (one frame at the drop position, invisible at 60fps; the old code's immediate `zone_rect` assignment is intentionally not replicated — note this if reviewing screenshots frame-by-frame).

- [ ] **Step 4: Delete `dwell_zone`/`dwell_start` fields** and their `new()` init; remove the now-unused `self.dwell_zone = None;` lines flagged by the compiler. `resolve_zone`/`detect_zone`/`lerp_rect`/`TOP_HOLD`/`GROW_LEAD` go in Task 12 (other code may still reference them until then — if nothing does, the compiler will say so; deleting early is fine if green).

- [ ] **Step 5: Build + full tests green.**
- [ ] **Step 6: Commit**: `git commit -am "feat(wm): drag tear-out and tree drop targets replace zone snapping"`

---

### Task 10: Resize handles → tree dividers

**Files:** Modify: `src/wm.rs` (`show` resize-handles loop)

- [ ] **Step 1: Replace the per-handle apply block.** Inside the `for (key, hr, hl, hrr, ht, hb, cursor) in handles` loop, replace the `match self.windows[i].snap { ... }` apply with:

```rust
                let d = resp.drag_delta();
                if self.zoomed == Some(id) {
                    continue; // zoomed windows don't resize
                }
                if self.tree.contains(id) {
                    let local = egui::Rect::from_min_size(egui::Pos2::ZERO, asz);
                    if hl {
                        self.tree.resize_edge(id, Dir::Left, d.x, local, SNAP_GAP);
                    }
                    if hrr {
                        self.tree.resize_edge(id, Dir::Right, d.x, local, SNAP_GAP);
                    }
                    if ht {
                        self.tree.resize_edge(id, Dir::Up, d.y, local, SNAP_GAP);
                    }
                    if hb {
                        self.tree.resize_edge(id, Dir::Down, d.y, local, SNAP_GAP);
                    }
                } else {
                    resize_floating(&mut self.windows[i].rect, d, hl, hrr, ht, hb, asz);
                }
```

Outer edges return `false` from `resize_edge` → no-op (decision §7). Corners on tiled windows naturally drive both axes.

- [ ] **Step 2: Build + tests green; commit**: `git commit -am "feat(wm): tiled resize drags tree dividers; outer edges inert"`

---

### Task 11: New windows tile by default

**Files:** Modify: `src/wm.rs` (`tile_new` helper + call sites), `src/main.rs` (startup spawn, if any)

- [ ] **Step 1: Failing test**

```rust
    #[test]
    fn new_terminal_tiles_by_default_splitting_the_focused_leaf() {
        // wm with tiled window A focused (insert_root manually), then run the
        // NewTerm path's placement: tile_new(b, Some(a)) →
        // tree.leaves() == [a, b]; a second tile_new(c, Some(b)) splits b's slot.
        // With nothing tiled and an empty tree: tile_new(d, None) → sole leaf.
    }
```

- [ ] **Step 2: Implement `tile_new`** (near `add_terminal`):

```rust
    /// Default placement for a freshly created window: split the anchor leaf
    /// (the previously-focused tiled window) along its longer axis; with no
    /// tiled anchor, enter at the root. The new window's floating rect is kept
    /// in `prev` for a later tear-out.
    fn tile_new(&mut self, id: WinId, anchor: Option<WinId>) {
        if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
            if w.prev.is_none() {
                w.prev = Some(w.rect);
            }
        }
        match anchor.filter(|a| *a != id && self.tree.contains(*a)) {
            Some(a) => {
                let r = self
                    .windows
                    .iter()
                    .find(|w| w.id == a)
                    .map(|w| w.rect)
                    .unwrap_or(egui::Rect::from_min_size(egui::Pos2::ZERO, self.last_area));
                let side = if r.width() >= r.height() { Dir::Right } else { Dir::Down };
                self.tree.insert_split(a, id, side);
            }
            None => self.tree.insert_root(id, Dir::Right),
        }
    }
```

- [ ] **Step 3: Wire call sites.** In each case capture the focused id BEFORE the window is created (creation steals focus):
  - `dispatch` `Command::NewTerm`: `let anchor = child.focused; if let Some(id) = child.add_terminal(Shell::PowerShell, &ctx) { child.tile_new(id, anchor); }`
  - `apply_acts` `Act::AddTerm(id, shell)`: same pattern inside the project's `wm`.
  - `add_terminal_cmd` (dispatched worker terminals): same pattern around its `add_terminal`/`push_win` call.
  - Project creation (picker accept — find via `find_referencing_symbols` on `add_project`) and any `main.rs` startup spawns: `tile_new` on the desktop manager with the prior focused project as anchor.
  - `open_chat_window`: **leave floating** (decision §5). `split_dir` already places via `place_split` — do NOT add `tile_new` there.

- [ ] **Step 4: Tests green; commit**: `git commit -am "feat(wm): new terminals and projects tile by default"`

---

### Task 12: Delete the zone system

**Files:** Modify: `src/wm.rs`, `src/keymap.rs` (only if `Dir::zone` imports leak), tests

- [ ] **Step 1: Delete in `src/wm.rs`** (Serena `safe_delete` where possible, compiler-driven otherwise): `Zone`, `detect_zone`, `zone_rect`, `compose_zone`, `interior_edges`, `resolve_zone`, `lerp_rect`, `set_snap`, `snap_or_tab`, `Dir::zone`, consts `TOP_HOLD`/`GROW_LEAD`, field `Win.snap` (keep `prev`!), field `split: egui::Vec2`, and the refit fallback `match w.snap {...}` from Task 5 (now just `clamp(&mut w.rect, asz)` in the else). Update `cr` to `if is_tiled || self.zoomed == Some(id) { ... }`. Update every struct literal (`push_win`, `untab`, tests) that still writes `snap:`.

- [ ] **Step 2: Delete/replace stale tests**: `snap_into_occupied_zone_tabs_onto_occupant`, `snap_into_empty_zone_just_snaps`, `compose_zone_matches_full_transition_table`, `dir_zone_and_opposite_mapping` (keep a trimmed `Dir::opposite` test only if `opposite` still has callers — if not, delete `opposite` too). Add one regression test:

```rust
    #[test]
    fn closing_a_tiled_window_collapses_its_slot() {
        // a|b tiled; close a (close_tab → close path) → tree.leaves() == [b]
        // and b's layout rect spans the full inner width next layout() call.
    }
```

- [ ] **Step 3: `cargo build` with zero warnings, full tests green.** Dead-code warnings = something missed.
- [ ] **Step 4: Commit**: `git commit -am "refactor(wm): delete the 9-zone snap system"`

---

### Task 13: Labels, help overlay, settings polish

**Files:** Modify: `src/keymap.rs` (`label`), `src/wm.rs` (`paint_help`)

- [ ] **Step 1:** `label()`: `TermSnap(d)` → "Move terminal left/down/up/right", `ProjSnap(d)` → "Move project …" (variant names unchanged — serialized user keybinding files keep resolving).
- [ ] **Step 2:** `paint_help`: replace the hardcoded "Corners" row (`"snap, then snap a perpendicular direction → quarter-screen tiles"`) with:

```rust
        rows.push((
            "  Drag".into(),
            "drag a header — leaf edges split, centers stack as tabs, screen edges make a column".into(),
        ));
```

- [ ] **Step 3:** Update the keymap test `wasd_snap_and_split_defaults` name/expectations if it asserts labels. Tests green.
- [ ] **Step 4: Commit**: `git commit -am "docs(keys): relabel snap→move, refresh help overlay"`

---

### Task 14: Visual verification

- [ ] **Step 1:** Temporarily spawn (in `main.rs` `if !self.started`) one project with three terminals; build, run, screenshot per `docs/HANDOFF.md` §3, `Read` the PNG. Expect: three tiles (one split pair per `tile_new`'s longer-axis rule), square corners, gaps, amber focus border.
- [ ] **Step 2:** Drive one drag via the HANDOFF Win32 `mouse_event` script (tell the user first — it moves their mouse): tear a tile out, watch siblings reflow, drop on a leaf's right half, screenshot the hint mid-drag and the result.
- [ ] **Step 3:** REVERT the temporary spawns. Re-run full tests. Commit only the (empty) result if anything real changed: this task normally produces no diff.

---

### Task 15: Docs

- [ ] **Step 1:** Create `docs/tiling-tree.md` (grug-simple): what the two states are, how drag/keys work, the `LayoutTree` shape, gotchas (outer-edge resize inert; chat viewer floats; zoom is an overlay), and a **Key files** section (`src/layout.rs`, `src/wm.rs`, `src/keymap.rs`).
- [ ] **Step 2:** `docs/epics/window-tabbing-split-epic.md`: add a banner under the title — superseded by the tree model on 2026-06-11 (user decision); §1's "we are NOT building a BSP tile tree" no longer holds; tabs/merge/untab phases still describe shipped behavior.
- [ ] **Step 3:** `docs/HANDOFF.md` §2 + architecture bullets: replace snap-zone description with tree+floating; note `src/layout.rs`. Same for the `CLAUDE.md` architecture summary lines and a short note in `docs/foreman.md`.
- [ ] **Step 4: Commit**: `git commit -am "docs: tiling tree feature doc; supersede zone-snap docs"`

---

### Task 16 (OPTIONAL — user decision pending): hover-reveal headers

⚠️ **Conflicts with HANDOFF §5.1** (status lines render in titlebars). Confirm with the user before implementing; skip freely.

Scope if approved: tiled, single-tab, non-project windows draw no titlebar; their content rect becomes the full window rect (terminal grid gains 26px, stable — no reflow on hover). When the pointer dwells ≥150ms in the top `TITLE_H` band, the full titlebar (drag rect, controls, rename) paints as an overlay on top of the grid. Floating, multi-tab, and project windows keep persistent headers (overlap legibility, tab bars, dispatch chips). Track dwell as `hdr_hover: Option<(WinId, f64)>` on `WindowManager`, mirroring the deleted `dwell_zone` pattern. Verify with screenshots; watch that header-band interacts are only registered while revealed, or top-row terminal text selection breaks.

---

## Self-review notes

- **Spec coverage:** two states ✓ (T1–T12), header-drag transitions ✓ (T9), insertion hints ✓ (T3/T9), tabs-in-tree ✓ (decision §2, no code needed beyond merge precedence in T9), both levels ✓ (engine-level change), Alt+WASD-always-tree ✓ (T8), tiled default ✓ (T11), float toggle keyboard path ✓ (T7/T8), hover header → T16 (flagged, pending), docs reversal ✓ (T15).
- **Known accepted gaps:** drop hints paint "through" an overlapping floating window (pointer hit-tests the tree, not z-order) — acceptable v1, noted in T15 doc; one-frame rect lag on drop commit (T9 step 3).
- **Type consistency check done:** `detach` (T5) used by T6/T8/T9; `tile_new(id, anchor)` (T11) matches all call sites; `toggle_zoom(id)` signature change propagated (T6); `place_split(Option<WinId>, WinId, Dir)` matches `split_dir` and tests.
