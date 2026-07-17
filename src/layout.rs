//! The tiling layout tree. Pure data + math — no egui interaction, no Session.
//! A `WindowManager` owns one `LayoutTree`; windows whose ids appear as leaves
//! are "tiled" and get their rects from `layout()` each frame. Windows absent
//! from the tree are floating.

use crate::wm::{Dir, WinId};
use eframe::egui;

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
        ratios: Vec<f32>, // same length as children, sums to 1.0
        children: Vec<Node>,
    },
}

fn holds(n: &Node, id: WinId) -> bool {
    match n {
        Node::Leaf(w) => *w == id,
        Node::Split { children, .. } => children.iter().any(|c| holds(c, id)),
    }
}

/// Deepest split on `axis` along the path to `id` where `edge` is interior:
/// (address-of-split, child-index, avail-extent-px). Shared by `resize_edge`
/// and `set_leaf_extent`.
fn find_interior_split(
    n: &Node,
    r: egui::Rect,
    id: WinId,
    edge: Dir,
    axis: SplitDir,
    gap: f32,
    addr: Vec<usize>,
) -> Option<(Vec<usize>, usize, f32)> {
    let Node::Split {
        dir,
        ratios,
        children,
    } = n
    else {
        return None;
    };
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
    let deeper = find_interior_split(&children[idx], child_rect, id, edge, axis, gap, child_addr);
    if deeper.is_some() {
        return deeper;
    }
    let interior = *dir == axis
        && match edge {
            Dir::Left | Dir::Up => idx > 0,
            Dir::Right | Dir::Down => idx < children.len() - 1,
        };
    interior.then_some((addr, idx, avail))
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
                Node::Split {
                    dir: SplitDir::of(side),
                    ratios: vec![0.5, 0.5],
                    children,
                }
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
            if let Node::Split {
                dir,
                ratios,
                children,
            } = n
            {
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
                *n = Node::Split {
                    dir: SplitDir::of(side),
                    ratios: vec![0.5, 0.5],
                    children,
                };
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
                Node::Split {
                    dir,
                    ratios,
                    children,
                } => {
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
                                    Node::Split {
                                        dir: cd,
                                        ratios: cr,
                                        children: cc,
                                    } if cd == dir => {
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
                            Some(Node::Split {
                                dir,
                                ratios: flat_r,
                                children: flat_c,
                            })
                        }
                    }
                }
            }
        }
        let mut removed = false;
        self.root = self.root.take().and_then(|r| prune(r, id, &mut removed));
        removed
    }

    /// Compute leaf rects within `area` (any coordinate space — local or screen),
    /// with `gap` pixels between siblings and as the outer margin (matches the
    /// old `zone_rect` SNAP_GAP geometry).
    pub fn layout(&self, area: egui::Rect, gap: f32) -> Vec<(WinId, egui::Rect)> {
        fn walk(n: &Node, r: egui::Rect, gap: f32, out: &mut Vec<(WinId, egui::Rect)>) {
            match n {
                Node::Leaf(w) => out.push((*w, r)),
                Node::Split {
                    dir,
                    ratios,
                    children,
                } => {
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

    /// The leaf whose rect (expanded to cover half the gap) contains `p`.
    pub fn hit_leaf(
        &self,
        p: egui::Pos2,
        area: egui::Rect,
        gap: f32,
    ) -> Option<(WinId, egui::Rect)> {
        self.layout(area, gap)
            .into_iter()
            .find(|(_, r)| r.expand(gap * 0.5 + 1.0).contains(p))
    }

    /// What inserting a window at `p` would do, plus the hint rect to paint.
    /// Precedence: area edge band (root split) → leaf center (tab) → leaf
    /// nearest-edge (split). Empty tree: edge band makes the first tile.
    pub fn drop_target(
        &self,
        p: egui::Pos2,
        area: egui::Rect,
        gap: f32,
    ) -> Option<(DropTarget, egui::Rect)> {
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
                Dir::Left => {
                    egui::Rect::from_min_max(inner.min, egui::pos2(inner.center().x, inner.max.y))
                }
                Dir::Right => {
                    egui::Rect::from_min_max(egui::pos2(inner.center().x, inner.min.y), inner.max)
                }
                Dir::Up => {
                    egui::Rect::from_min_max(inner.min, egui::pos2(inner.max.x, inner.center().y))
                }
                Dir::Down => {
                    egui::Rect::from_min_max(egui::pos2(inner.min.x, inner.center().y), inner.max)
                }
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

    /// Whether leaf `id` has a draggable divider on `edge` — i.e. some ancestor
    /// split runs on that axis and the edge is interior there. The resize-handle
    /// cursor only advertises edges where this is true (outer edges are inert).
    pub fn has_divider(&self, id: WinId, edge: Dir) -> bool {
        let axis = SplitDir::of(edge);
        fn holds(n: &Node, id: WinId) -> bool {
            match n {
                Node::Leaf(w) => *w == id,
                Node::Split { children, .. } => children.iter().any(|c| holds(c, id)),
            }
        }
        // None = subtree doesn't contain `id`; Some(found) = contains it, and
        // `found` says whether a matching interior divider exists at or below.
        fn go(n: &Node, id: WinId, edge: Dir, axis: SplitDir) -> Option<bool> {
            let Node::Split { dir, children, .. } = n else {
                return matches!(n, Node::Leaf(w) if *w == id).then_some(false);
            };
            let idx = children.iter().position(|c| holds(c, id))?;
            if go(&children[idx], id, edge, axis) == Some(true) {
                return Some(true);
            }
            let interior = *dir == axis
                && match edge {
                    Dir::Left | Dir::Up => idx > 0,
                    Dir::Right | Dir::Down => idx < children.len() - 1,
                };
            Some(interior)
        }
        self.root
            .as_ref()
            .and_then(|r| go(r, id, edge, axis))
            .unwrap_or(false)
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
        self.resize_edge_inner(id, edge, delta_px, area, gap, None)
    }

    /// Like [`Self::resize_edge`], but when leaf `soft.0` sits directly on
    /// either side of the resolved divider, its floor is `soft.1` px instead
    /// of MIN_RATIO. The task-manager panel is pinned below MIN_RATIO on wide
    /// desktops (`set_leaf_extent`), so the plain clamp ratchets: a drag that
    /// grew the panel could never shrink it back past 10% of the desktop.
    pub fn resize_edge_soft_min(
        &mut self,
        id: WinId,
        edge: Dir,
        delta_px: f32,
        area: egui::Rect,
        gap: f32,
        soft: (WinId, f32),
    ) -> bool {
        self.resize_edge_inner(id, edge, delta_px, area, gap, Some(soft))
    }

    fn resize_edge_inner(
        &mut self,
        id: WinId,
        edge: Dir,
        delta_px: f32,
        area: egui::Rect,
        gap: f32,
        soft: Option<(WinId, f32)>,
    ) -> bool {
        let axis = SplitDir::of(edge);
        let found = match &self.root {
            Some(r) => find_interior_split(r, area.shrink(gap), id, edge, axis, gap, Vec::new()),
            None => None,
        };
        let Some((addr, idx, avail)) = found else {
            return false;
        };
        // Descend by address and adjust the two ratios.
        let mut node = self.root.as_mut().unwrap();
        for i in addr {
            let Node::Split { children, .. } = node else {
                unreachable!()
            };
            node = &mut children[i];
        }
        let Node::Split {
            ratios, children, ..
        } = node
        else {
            unreachable!()
        };
        let (a, b) = match edge {
            Dir::Left | Dir::Up => (idx - 1, idx),
            Dir::Right | Dir::Down => (idx, idx + 1),
        };
        let min_of = |i: usize| match soft {
            Some((sid, px)) if matches!(children[i], Node::Leaf(w) if w == sid) => px / avail,
            _ => MIN_RATIO,
        };
        let lo = (min_of(a) - ratios[a]).min(0.0);
        let hi = (ratios[b] - min_of(b)).max(0.0);
        let df = (delta_px / avail).clamp(lo, hi);
        ratios[a] += df;
        ratios[b] -= df;
        true
    }

    /// Pin leaf `id`'s width to `target_px`: `set_leaf_extent` on the H axis.
    pub fn set_leaf_width(
        &mut self,
        id: WinId,
        target_px: f32,
        area: egui::Rect,
        gap: f32,
    ) -> bool {
        self.set_leaf_extent(id, SplitDir::H, target_px, area, gap)
    }

    /// Pin leaf `id`'s extent along `axis` (H = width, V = height) to
    /// `target_px` by moving the divider it shares with its nearest sibling on
    /// that axis. Unlike `resize_edge`, the pinned leaf may drop below
    /// MIN_RATIO (the collapsed task-manager rail is far narrower than any
    /// tile); the sibling still clamps at MIN_RATIO. False when the leaf has
    /// no interior divider on `axis` (sole leaf / empty tree / axis unused
    /// anywhere above the leaf).
    pub fn set_leaf_extent(
        &mut self,
        id: WinId,
        axis: SplitDir,
        target_px: f32,
        area: egui::Rect,
        gap: f32,
    ) -> bool {
        let edges = match axis {
            SplitDir::H => [Dir::Right, Dir::Left],
            SplitDir::V => [Dir::Down, Dir::Up],
        };
        for edge in edges {
            let found = match &self.root {
                Some(r) => {
                    find_interior_split(r, area.shrink(gap), id, edge, axis, gap, Vec::new())
                }
                None => None,
            };
            let Some((addr, idx, avail)) = found else {
                continue;
            };
            let mut node = self.root.as_mut().unwrap();
            for i in addr {
                let Node::Split { children, .. } = node else {
                    unreachable!()
                };
                node = &mut children[i];
            }
            let Node::Split { ratios, .. } = node else {
                unreachable!()
            };
            let floor = (2.0 / avail).min(MIN_RATIO);
            let want = target_px / avail - ratios[idx];
            let (a, b, lo, hi, desired) = match edge {
                Dir::Right | Dir::Down => (
                    idx,
                    idx + 1,
                    floor - ratios[idx],
                    ratios[idx + 1] - MIN_RATIO,
                    want,
                ),
                Dir::Left | Dir::Up => (
                    idx - 1,
                    idx,
                    MIN_RATIO - ratios[idx - 1],
                    ratios[idx] - floor,
                    -want,
                ),
            };
            let df = desired.clamp(lo.min(hi), hi.max(lo));
            ratios[a] += df;
            ratios[b] -= df;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Task 1: tree core ────────────────────────────────────────────────────

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
            Node::Split {
                dir,
                ratios,
                children,
            } => {
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
        // Force a nested same-dir tree (insert_split alone keeps things flat):
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
        t.remove(2);
        // nested single-child H collapsed; tree is a 2-way H split [1, 3]
        match t.root.as_ref().unwrap() {
            Node::Split { children, .. } => assert_eq!(children.len(), 2),
            _ => panic!(),
        }
        assert_eq!(t.leaves(), vec![1, 3]);
    }

    // ── Task 2: rect computation ─────────────────────────────────────────────

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

    // ── Task 3: hit-testing and drop targets ─────────────────────────────────

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
        let (tgt, _) = t
            .drop_target(egui::pos2(500.0, 400.0), area(), 8.0)
            .unwrap();
        assert_eq!(tgt, DropTarget::Tab(1));
        let (tgt, _) = t
            .drop_target(egui::pos2(200.0, 400.0), area(), 8.0)
            .unwrap();
        assert_eq!(tgt, DropTarget::Split(1, Dir::Left));
        let (tgt, _) = t
            .drop_target(egui::pos2(500.0, 700.0), area(), 8.0)
            .unwrap();
        assert_eq!(tgt, DropTarget::Split(1, Dir::Down));
    }

    #[test]
    fn drop_target_area_edge_band_splits_the_root() {
        let mut t = LayoutTree::default();
        t.insert_root(1, Dir::Right);
        let (tgt, _) = t.drop_target(egui::pos2(10.0, 400.0), area(), 8.0).unwrap();
        assert_eq!(tgt, DropTarget::Root(Dir::Left));
        let (tgt, _) = t
            .drop_target(egui::pos2(500.0, 795.0), area(), 8.0)
            .unwrap();
        assert_eq!(tgt, DropTarget::Root(Dir::Down));
    }

    #[test]
    fn drop_target_on_empty_tree_uses_edge_band_only() {
        let t = LayoutTree::default();
        assert!(
            t.drop_target(egui::pos2(500.0, 400.0), area(), 8.0)
                .is_none()
        ); // center: nothing
        let (tgt, hint) = t.drop_target(egui::pos2(10.0, 400.0), area(), 8.0).unwrap();
        assert!(matches!(tgt, DropTarget::Root(_)));
        assert!((hint.width() - 984.0).abs() < 0.01); // full inner area
    }

    // ── Task 4: swap and resize_edge ─────────────────────────────────────────

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
    fn resize_edge_soft_min_can_shrink_a_pinned_leaf_back_below_min_ratio() {
        // Repro: the Sessions panel is pinned to 260px on a wide desktop —
        // below MIN_RATIO — a drag grows it, and the reverse drag must bring
        // it back; plain resize_edge ratchets at MIN_RATIO (10% ≈ 298px here).
        let wide = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(3000.0, 800.0));
        let mut t = LayoutTree::default();
        t.insert_root(1, Dir::Right);
        t.insert_split(1, 2, Dir::Right); // [project | panel]
        assert!(t.set_leaf_width(2, 260.0, wide, 8.0));
        let width_of = |t: &LayoutTree, id: WinId| {
            t.layout(wide, 8.0)
                .into_iter()
                .find(|(w, _)| *w == id)
                .unwrap()
                .1
                .width()
        };
        assert!((width_of(&t, 2) - 260.0).abs() < 0.5);
        let soft = (2, 76.0);
        // Grow the panel by dragging its left edge 160px left…
        assert!(t.resize_edge_soft_min(2, Dir::Left, -160.0, wide, 8.0, soft));
        assert!((width_of(&t, 2) - 420.0).abs() < 0.5);
        // …and back: must return to 260, not stop at MIN_RATIO.
        assert!(t.resize_edge_soft_min(2, Dir::Left, 160.0, wide, 8.0, soft));
        let w = width_of(&t, 2);
        assert!((w - 260.0).abs() < 0.5, "ratcheted at {w}");
        // Over-shrinking clamps at the soft floor, not MIN_RATIO.
        t.resize_edge_soft_min(2, Dir::Left, 100_000.0, wide, 8.0, soft);
        let w = width_of(&t, 2);
        assert!((w - 76.0).abs() < 1.0, "floor got {w}");
        // The non-soft side keeps its MIN_RATIO guarantee.
        t.resize_edge_soft_min(2, Dir::Left, -100_000.0, wide, 8.0, soft);
        assert!(width_of(&t, 1) >= 2976.0 * MIN_RATIO - 0.5);
    }

    #[test]
    fn resize_edge_with_degenerate_ratios_does_not_panic() {
        // Repeated same-axis splits halve ratios below MIN_RATIO: 0.5, 0.25, 0.125, 0.0625, 0.0625
        let mut t = LayoutTree::default();
        t.insert_root(1, Dir::Right);
        t.insert_split(1, 2, Dir::Right);
        t.insert_split(2, 3, Dir::Right);
        t.insert_split(3, 4, Dir::Right);
        t.insert_split(4, 5, Dir::Right);
        // both ratios at this divider are < MIN_RATIO; must not panic either direction
        assert!(t.resize_edge(4, Dir::Right, 50.0, area(), 8.0));
        assert!(t.resize_edge(4, Dir::Right, -50.0, area(), 8.0));
        let total: f32 = match t.root.as_ref().unwrap() {
            Node::Split { ratios, .. } => ratios.iter().sum(),
            _ => panic!(),
        };
        assert!((total - 1.0).abs() < 1e-3); // ratios still normalized
    }

    #[test]
    fn resize_outer_edge_is_a_noop_and_min_ratio_clamps() {
        let mut t = LayoutTree::default();
        t.insert_root(1, Dir::Right);
        t.insert_split(1, 2, Dir::Right);
        assert!(!t.resize_edge(1, Dir::Left, 50.0, area(), 8.0)); // 1 owns no left divider
        assert!(!t.resize_edge(1, Dir::Up, 50.0, area(), 8.0)); // no vertical split at all
        t.resize_edge(1, Dir::Right, 100_000.0, area(), 8.0); // absurd drag
        let p = t.layout(area(), 8.0);
        let r2 = p.iter().find(|(w, _)| *w == 2).unwrap().1;
        assert!(r2.width() >= 976.0 * MIN_RATIO - 0.5); // clamped, not crushed
    }

    #[test]
    fn has_divider_reflects_interior_edges_only() {
        let mut t = LayoutTree::default();
        t.insert_root(1, Dir::Right);
        t.insert_split(1, 2, Dir::Right); // [1 | 2]
        assert!(t.has_divider(1, Dir::Right));
        assert!(t.has_divider(2, Dir::Left));
        assert!(!t.has_divider(1, Dir::Left)); // outer
        assert!(!t.has_divider(1, Dir::Up)); // no V split anywhere
        assert!(!t.has_divider(99, Dir::Left)); // not in tree
        t.insert_split(2, 3, Dir::Down); // right column = V[2, 3]
        assert!(t.has_divider(2, Dir::Down));
        assert!(t.has_divider(3, Dir::Up));
        assert!(t.has_divider(3, Dir::Left)); // column's left edge is the root divider
        assert!(!t.has_divider(3, Dir::Right)); // outer
    }

    #[test]
    fn set_leaf_width_pins_a_leaf_below_min_ratio() {
        let mut t = LayoutTree::default();
        t.insert_root(1, Dir::Right);
        t.insert_split(1, 2, Dir::Right); // [1 | 2]
        assert!(t.set_leaf_width(2, 36.0, area(), 8.0));
        let p = t.layout(area(), 8.0);
        let r2 = p.iter().find(|(w, _)| *w == 2).unwrap().1;
        assert!((r2.width() - 36.0).abs() < 0.5, "got {}", r2.width());
    }

    #[test]
    fn set_leaf_width_reaches_a_leaf_nested_in_a_v_column() {
        let mut t = LayoutTree::default();
        t.insert_root(1, Dir::Right);
        t.insert_split(1, 2, Dir::Right);
        t.insert_split(2, 3, Dir::Down); // right column stacks 2 over 3
        assert!(t.set_leaf_width(2, 200.0, area(), 8.0));
        let p = t.layout(area(), 8.0);
        let r2 = p.iter().find(|(w, _)| *w == 2).unwrap().1;
        let r3 = p.iter().find(|(w, _)| *w == 3).unwrap().1;
        assert!((r2.width() - 200.0).abs() < 0.5, "got {}", r2.width());
        assert!((r3.width() - 200.0).abs() < 0.5); // same column follows
    }

    #[test]
    fn set_leaf_width_is_false_for_a_sole_leaf_and_clamps_the_sibling() {
        let mut t = LayoutTree::default();
        t.insert_root(1, Dir::Right);
        assert!(!t.set_leaf_width(1, 36.0, area(), 8.0));
        // [1 | 2]: growing 2 to nearly everything leaves 1 at MIN_RATIO
        t.insert_split(1, 2, Dir::Right);
        assert!(t.set_leaf_width(2, 950.0, area(), 8.0));
        let p = t.layout(area(), 8.0);
        let r1 = p.iter().find(|(w, _)| *w == 1).unwrap().1;
        assert!((r1.width() - 97.6).abs() < 0.5, "got {}", r1.width());
    }

    // ── set_leaf_extent, V axis (mirrors the set_leaf_width trio) ───────────

    #[test]
    fn set_leaf_extent_v_pins_a_leaf_below_min_ratio() {
        let mut t = LayoutTree::default();
        t.insert_root(1, Dir::Right);
        t.insert_split(1, 2, Dir::Down); // 1 stacked over 2
        assert!(t.set_leaf_extent(2, SplitDir::V, 36.0, area(), 8.0));
        let p = t.layout(area(), 8.0);
        let r2 = p.iter().find(|(w, _)| *w == 2).unwrap().1;
        assert!((r2.height() - 36.0).abs() < 0.5, "got {}", r2.height());
    }

    #[test]
    fn set_leaf_extent_v_reaches_a_leaf_nested_in_an_h_row() {
        let mut t = LayoutTree::default();
        t.insert_root(1, Dir::Right);
        t.insert_split(1, 2, Dir::Down);
        t.insert_split(2, 3, Dir::Right); // bottom row sits 2 beside 3
        assert!(t.set_leaf_extent(2, SplitDir::V, 200.0, area(), 8.0));
        let p = t.layout(area(), 8.0);
        let r2 = p.iter().find(|(w, _)| *w == 2).unwrap().1;
        let r3 = p.iter().find(|(w, _)| *w == 3).unwrap().1;
        assert!((r2.height() - 200.0).abs() < 0.5, "got {}", r2.height());
        assert!((r3.height() - 200.0).abs() < 0.5); // same row follows
    }

    #[test]
    fn set_leaf_extent_v_is_false_for_a_sole_leaf_and_clamps_the_sibling() {
        let mut t = LayoutTree::default();
        t.insert_root(1, Dir::Right);
        assert!(!t.set_leaf_extent(1, SplitDir::V, 36.0, area(), 8.0));
        // [1 / 2]: growing 2 to nearly everything leaves 1 at MIN_RATIO
        t.insert_split(1, 2, Dir::Down);
        assert!(t.set_leaf_extent(2, SplitDir::V, 750.0, area(), 8.0));
        let p = t.layout(area(), 8.0);
        let r1 = p.iter().find(|(w, _)| *w == 1).unwrap().1;
        assert!((r1.height() - 77.6).abs() < 0.5, "got {}", r1.height());
    }

    #[test]
    fn set_leaf_extent_v_is_false_when_only_h_dividers_exist() {
        // The axis probe is what lets the wm try H then fall back to V:
        // an H-only tree must refuse a V pin (and vice versa).
        let mut t = LayoutTree::default();
        t.insert_root(1, Dir::Right);
        t.insert_split(1, 2, Dir::Right); // [1 | 2] — no V split anywhere
        assert!(!t.set_leaf_extent(2, SplitDir::V, 36.0, area(), 8.0));
        assert!(t.set_leaf_extent(2, SplitDir::H, 36.0, area(), 8.0));
    }
}
