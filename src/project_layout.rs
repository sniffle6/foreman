//! Project layout model: the GUI-free arrangement of a Project's child
//! windows (tab stacks, tiling tree, floating and minimized placement) keyed
//! by persistent layout-item identity, plus validation, deterministic
//! reconciliation against the live inventory, Modified comparison, and
//! normalized-geometry adaptation. Spec:
//! `docs/superpowers/specs/2026-10-16-project-layouts-design.md`.
//!
//! Nothing here touches egui, `Content`, or a `WindowManager`. The manager
//! owns Content and applies a validated `Arrangement`; this module only
//! decides what that arrangement is. Every public entry point validates its
//! input and never returns an arrangement that fails [`validate`].

// The model ships ahead of its consumers (project-layouts plan: persistence,
// Apply/Revert, picker). Until those waves land only the tests call it;
// remove this once `WindowManager` applies an `Arrangement`.
#![cfg_attr(not(test), allow(dead_code))]

use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;

/// Persistent identity of one Content-bearing tab inside a Project. Minted
/// once at tab construction from a process-global counter, carried through
/// tab moves, and restored verbatim from the workspace snapshot. Independent
/// of the runtime `WinId`, the terminal Member id, and the containing stack.
pub type LayoutItemId = u64;

/// Upper bound on items in one definition or inventory.
pub const MAX_ITEMS: usize = 256;
/// Upper bound on nested split depth (root leaf = depth 1).
pub const MAX_TREE_DEPTH: usize = 16;
/// Layout names are trimmed and limited to this many characters (spec §Save).
pub const MAX_NAME_CHARS: usize = 80;
/// Share of the width given to the **Additional windows** stack when it is
/// appended beside surviving saved geometry.
pub const ADDITIONAL_RATIO: f32 = 1.0 / 3.0;
/// Tolerance for ratio and normalized-rect comparison in [`same_arrangement`].
const GEOM_EPS: f32 = 1e-3;

/// What kind of Content a layout item is. Reconciliation substitutes only
/// within one kind; a viewer never stands in for a terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ItemKind {
    Terminal,
    Chat,
    Board,
    Plan,
    GitHistory,
    GitChanges,
    GitDiff,
    Image,
    Project,
}

impl ItemKind {
    /// Whether Apply may open a fresh item of this kind for a missing slot:
    /// a terminal on the default shell in the Project directory, or a view
    /// built from Project state alone (`WindowManager::open_*_window`).
    /// Image, Git diff, and nested Project need a target.
    pub fn opens_without_target(self) -> bool {
        use ItemKind::*;
        matches!(
            self,
            Terminal | Chat | Board | Plan | GitHistory | GitChanges
        )
    }

    /// One per Project: the open path surfaces the existing viewer instead
    /// of opening a second, so reconciliation never queues one beside it.
    fn is_project_singleton(self) -> bool {
        use ItemKind::*;
        matches!(self, Chat | Board | Plan | GitHistory | GitChanges)
    }
}

/// One layout item: its persistent id and Content kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemRef {
    pub id: LayoutItemId,
    pub kind: ItemKind,
}

impl ItemRef {
    pub const fn new(id: LayoutItemId, kind: ItemKind) -> Self {
        Self { id, kind }
    }
}

/// A rect as fractions of the Project's content area (0.0..=1.0 is inside).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// A rect in manager-local pixels (origin = the manager area's min corner).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LocalRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Width and height in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Size {
    pub w: f32,
    pub h: f32,
}

impl Size {
    pub const fn new(w: f32, h: f32) -> Self {
        Self { w, h }
    }
}

impl NormRect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    /// Normalize a local rect against `area`. A degenerate area (zero or
    /// negative extent) is treated as 1px so the result stays finite.
    pub fn from_local(r: LocalRect, area: Size) -> Self {
        let aw = area.w.max(1.0);
        let ah = area.h.max(1.0);
        Self {
            x: r.x / aw,
            y: r.y / ah,
            w: r.w / aw,
            h: r.h / ah,
        }
    }

    /// Denormalize against `area`, then enforce the floating-size floor `min`
    /// (capped at the area itself) and confine the rect to the area.
    pub fn to_local(&self, area: Size, min: Size) -> LocalRect {
        let aw = area.w.max(0.0);
        let ah = area.h.max(0.0);
        let w = (self.w * aw).max(min.w).min(aw);
        let h = (self.h * ah).max(min.h).min(ah);
        let x = (self.x * aw).clamp(0.0, (aw - w).max(0.0));
        let y = (self.y * ah).clamp(0.0, (ah - h).max(0.0));
        LocalRect { x, y, w, h }
    }

    fn is_finite(&self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.w.is_finite() && self.h.is_finite()
    }
}

/// Where a window lives. Mirrors the manager's two states plus the minimized
/// overlay: a minimized window remembers whether it re-enters the tree on
/// restore and the floating rect it would otherwise take.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", deny_unknown_fields)]
pub enum WindowState {
    /// A leaf of the tree. The tree gives the rect.
    Tiled,
    Floating {
        rect: NormRect,
    },
    Minimized {
        from_tree: bool,
        rect: NormRect,
    },
}

/// One tab stack. `tabs` is the tab order; `active` indexes it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowSpec {
    pub tabs: Vec<LayoutItemId>,
    pub active: usize,
    pub state: WindowState,
}

impl WindowSpec {
    pub fn tiled(tabs: Vec<LayoutItemId>, active: usize) -> Self {
        Self {
            tabs,
            active,
            state: WindowState::Tiled,
        }
    }

    pub fn floating(tabs: Vec<LayoutItemId>, active: usize, rect: NormRect) -> Self {
        Self {
            tabs,
            active,
            state: WindowState::Floating { rect },
        }
    }

    pub fn minimized(
        tabs: Vec<LayoutItemId>,
        active: usize,
        from_tree: bool,
        rect: NormRect,
    ) -> Self {
        Self {
            tabs,
            active,
            state: WindowState::Minimized { from_tree, rect },
        }
    }

    fn is_tiled(&self) -> bool {
        matches!(self.state, WindowState::Tiled)
    }
}

/// Split direction. `H` lays children left→right, `V` top→bottom (same
/// meaning as `layout::SplitDir`, kept separate so the model stays serde-only).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SplitAxis {
    H,
    V,
}

/// The tiling tree. Leaves index `Arrangement::windows`; a leaf's window must
/// be [`WindowState::Tiled`] and every tiled window appears exactly once.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum TreeSpec {
    Leaf {
        window: usize,
    },
    Split {
        dir: SplitAxis,
        ratios: Vec<f32>,
        children: Vec<TreeSpec>,
    },
}

/// A complete arrangement of a Project's windows. `windows` is stacking order
/// (low index = back, high = front) among floating windows; tiled and
/// minimized windows take their order from the tree and the inventory.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Arrangement {
    pub windows: Vec<WindowSpec>,
    pub tree: Option<TreeSpec>,
}

/// A saved, named layout. `id` is immutable; `name` is user-editable.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayoutDefinition {
    pub id: String,
    pub name: String,
    pub items: Vec<ItemRef>,
    pub arrangement: Arrangement,
}

/// Why an arrangement, definition, inventory, or name was rejected.
#[derive(Clone, Debug, PartialEq)]
pub enum LayoutError {
    TooManyItems(usize),
    DuplicateItem(LayoutItemId),
    UnknownItem(LayoutItemId),
    /// An item of the definition appears in no window.
    MissingItem(LayoutItemId),
    EmptyStack(usize),
    BadActiveTab {
        window: usize,
        active: usize,
    },
    TreeTooDeep,
    LeafOutOfRange(usize),
    LeafNotTiled(usize),
    DuplicateLeaf(usize),
    TiledWindowNotInTree(usize),
    /// A split with fewer than two children or mismatched ratio count.
    MalformedSplit,
    NonFiniteRatio,
    NonFiniteRect(usize),
    DegenerateRect(usize),
    EmptyName,
    NameTooLong(usize),
    /// A fresh item reported for a slot is not the slot's kind.
    KindMismatch(LayoutItemId),
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyItems(n) => write!(f, "too many items ({n}; limit {MAX_ITEMS})"),
            Self::DuplicateItem(id) => write!(f, "item {id} appears more than once"),
            Self::UnknownItem(id) => write!(f, "item {id} is not in the inventory"),
            Self::MissingItem(id) => write!(f, "item {id} is not placed in any window"),
            Self::EmptyStack(w) => write!(f, "window {w} has no tabs"),
            Self::BadActiveTab { window, active } => {
                write!(f, "window {window} active tab {active} is out of range")
            }
            Self::TreeTooDeep => write!(f, "tree deeper than {MAX_TREE_DEPTH}"),
            Self::LeafOutOfRange(w) => write!(f, "tree leaf references missing window {w}"),
            Self::LeafNotTiled(w) => write!(f, "tree leaf references non-tiled window {w}"),
            Self::DuplicateLeaf(w) => write!(f, "window {w} appears twice in the tree"),
            Self::TiledWindowNotInTree(w) => write!(f, "tiled window {w} is not in the tree"),
            Self::MalformedSplit => write!(f, "split needs 2+ children and one ratio each"),
            Self::NonFiniteRatio => write!(f, "split ratio is not a finite positive number"),
            Self::NonFiniteRect(w) => write!(f, "window {w} rect is not finite"),
            Self::DegenerateRect(w) => write!(f, "window {w} rect has no area"),
            Self::EmptyName => write!(f, "layout name is empty"),
            Self::NameTooLong(n) => {
                write!(f, "layout name is {n} characters; limit {MAX_NAME_CHARS}")
            }
            Self::KindMismatch(id) => write!(f, "item {id} is not the kind its slot needs"),
        }
    }
}

impl std::error::Error for LayoutError {}

/// Trim a layout name and enforce the length contract. Returns the trimmed name.
pub fn validate_name(name: &str) -> Result<String, LayoutError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(LayoutError::EmptyName);
    }
    let n = trimmed.chars().count();
    if n > MAX_NAME_CHARS {
        return Err(LayoutError::NameTooLong(n));
    }
    Ok(trimmed.to_string())
}

/// True when two names collide under the Project's case-insensitive rule.
pub fn names_collide(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// Reject an item list with duplicates or more than [`MAX_ITEMS`] entries.
pub fn validate_items(items: &[ItemRef]) -> Result<(), LayoutError> {
    if items.len() > MAX_ITEMS {
        return Err(LayoutError::TooManyItems(items.len()));
    }
    let mut seen = HashSet::with_capacity(items.len());
    for item in items {
        if !seen.insert(item.id) {
            return Err(LayoutError::DuplicateItem(item.id));
        }
    }
    Ok(())
}

/// Full structural validation of `arrangement` against `items`: every item
/// placed exactly once, no unknown ids, nonempty stacks with a valid active
/// tab, a bounded tree whose leaves are exactly the tiled windows, finite
/// positive ratios, and finite non-degenerate rects.
pub fn validate(arrangement: &Arrangement, items: &[ItemRef]) -> Result<(), LayoutError> {
    validate_items(items)?;
    let known: HashSet<LayoutItemId> = items.iter().map(|i| i.id).collect();
    let mut placed: HashSet<LayoutItemId> = HashSet::with_capacity(items.len());
    for (wi, w) in arrangement.windows.iter().enumerate() {
        if w.tabs.is_empty() {
            return Err(LayoutError::EmptyStack(wi));
        }
        if w.active >= w.tabs.len() {
            return Err(LayoutError::BadActiveTab {
                window: wi,
                active: w.active,
            });
        }
        for &id in &w.tabs {
            if !known.contains(&id) {
                return Err(LayoutError::UnknownItem(id));
            }
            if !placed.insert(id) {
                return Err(LayoutError::DuplicateItem(id));
            }
        }
        match &w.state {
            WindowState::Tiled => {}
            WindowState::Floating { rect } | WindowState::Minimized { rect, .. } => {
                if !rect.is_finite() {
                    return Err(LayoutError::NonFiniteRect(wi));
                }
                if rect.w <= 0.0 || rect.h <= 0.0 {
                    return Err(LayoutError::DegenerateRect(wi));
                }
            }
        }
    }
    if let Some(item) = items.iter().find(|i| !placed.contains(&i.id)) {
        return Err(LayoutError::MissingItem(item.id));
    }

    let mut in_tree: HashSet<usize> = HashSet::new();
    if let Some(tree) = &arrangement.tree {
        validate_tree(tree, &arrangement.windows, &mut in_tree, 1)?;
    }
    for (wi, w) in arrangement.windows.iter().enumerate() {
        if w.is_tiled() && !in_tree.contains(&wi) {
            return Err(LayoutError::TiledWindowNotInTree(wi));
        }
    }
    Ok(())
}

fn validate_tree(
    node: &TreeSpec,
    windows: &[WindowSpec],
    in_tree: &mut HashSet<usize>,
    depth: usize,
) -> Result<(), LayoutError> {
    if depth > MAX_TREE_DEPTH {
        return Err(LayoutError::TreeTooDeep);
    }
    match node {
        TreeSpec::Leaf { window } => {
            let Some(w) = windows.get(*window) else {
                return Err(LayoutError::LeafOutOfRange(*window));
            };
            if !w.is_tiled() {
                return Err(LayoutError::LeafNotTiled(*window));
            }
            if !in_tree.insert(*window) {
                return Err(LayoutError::DuplicateLeaf(*window));
            }
            Ok(())
        }
        TreeSpec::Split {
            ratios, children, ..
        } => {
            if children.len() < 2 || ratios.len() != children.len() {
                return Err(LayoutError::MalformedSplit);
            }
            if ratios.iter().any(|r| !r.is_finite() || *r <= 0.0) {
                return Err(LayoutError::NonFiniteRatio);
            }
            for c in children {
                validate_tree(c, windows, in_tree, depth + 1)?;
            }
            Ok(())
        }
    }
}

/// A saved slot filled by a current item other than the saved one: a
/// same-kind substitute, or a fresh item opened for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Substitution {
    pub saved: LayoutItemId,
    pub current: LayoutItemId,
    pub kind: ItemKind,
}

/// A saved slot Apply fills by opening a fresh item of `saved.kind`.
/// `placeholder` holds the slot in `Reconciliation::arrangement`; it is
/// never the id of a current item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenSlot {
    pub placeholder: LayoutItemId,
    pub saved: ItemRef,
}

/// The deterministic result of fitting a definition to the current inventory.
/// `arrangement` is validated against `items` and places each exactly once.
#[derive(Clone, Debug, PartialEq)]
pub struct Reconciliation {
    pub arrangement: Arrangement,
    /// What `arrangement` places: the inventory, then one placeholder per
    /// `to_open` slot (the opened items once [`Self::with_opened`] ran).
    pub items: Vec<ItemRef>,
    /// Saved items found by exact id (and kind).
    pub matched: Vec<LayoutItemId>,
    pub substituted: Vec<Substitution>,
    /// Missing tiled or floating slots of a kind that opens without a target.
    /// Apply opens one fresh item per slot, then calls [`Self::with_opened`].
    pub to_open: Vec<OpenSlot>,
    /// Slots filled by fresh items; set by [`Self::with_opened`].
    pub opened: Vec<Substitution>,
    /// Visible current items with no slot, in inventory order: the appended
    /// **Additional windows** stack.
    pub additional: Vec<ItemRef>,
    /// Minimized current items with no slot, in inventory order. They stay
    /// minimized in their own stack with their restore placement.
    pub kept_minimized: Vec<ItemRef>,
    /// Saved slots left empty and omitted: kinds that need a target,
    /// minimized slots, a singleton viewer the Project already has, and
    /// failed opens.
    pub unfilled: Vec<ItemRef>,
}

impl Reconciliation {
    /// The Apply is clean and leaves Modified clear: every slot was filled
    /// and nothing went to Additional windows. Substitutes and kept-minimized
    /// extras do not count; pending opens do until [`Self::with_opened`].
    pub fn is_exact(&self) -> bool {
        self.to_open.is_empty() && self.unfilled.is_empty() && self.additional.is_empty()
    }

    /// Resolve `to_open` once Apply has opened fresh items. `opened` pairs a
    /// placeholder with the item opened for it. A slot with no pair failed
    /// to open: it leaves the arrangement the way an unfilled saved slot
    /// does and joins `unfilled`. Rejects an unknown or repeated placeholder,
    /// an item of the wrong kind, and an id already in the inventory.
    pub fn with_opened(
        &self,
        opened: &[(LayoutItemId, ItemRef)],
    ) -> Result<Reconciliation, LayoutError> {
        let slots: HashMap<LayoutItemId, ItemRef> = self
            .to_open
            .iter()
            .map(|o| (o.placeholder, o.saved))
            .collect();
        let mut fresh: HashMap<LayoutItemId, ItemRef> = HashMap::new();
        for &(placeholder, item) in opened {
            let Some(saved) = slots.get(&placeholder) else {
                return Err(LayoutError::UnknownItem(placeholder));
            };
            if item.kind != saved.kind {
                return Err(LayoutError::KindMismatch(item.id));
            }
            if fresh.insert(placeholder, item).is_some() {
                return Err(LayoutError::DuplicateItem(placeholder));
            }
        }

        let mut next = self.clone();
        next.to_open.clear();
        next.items.retain(|i| !slots.contains_key(&i.id));
        for slot in &self.to_open {
            match fresh.get(&slot.placeholder) {
                Some(item) => {
                    next.items.push(*item);
                    next.opened.push(Substitution {
                        saved: slot.saved.id,
                        current: item.id,
                        kind: item.kind,
                    });
                }
                None => next.unfilled.push(slot.saved),
            }
        }
        let (windows, index_map) = remap_windows(&self.arrangement.windows, |id| {
            if slots.contains_key(&id) {
                fresh.get(&id).map(|i| i.id)
            } else {
                Some(id)
            }
        });
        let tree = self
            .arrangement
            .tree
            .as_ref()
            .and_then(|t| prune_tree(t, &index_map));
        next.arrangement = Arrangement { windows, tree };
        validate(&next.arrangement, &next.items)?;
        Ok(next)
    }
}

/// Fit `def` to `inventory` (current items in arrangement order), given the
/// live `current` arrangement of those items.
///
/// 1. Exact id+kind matches first.
/// 2. Remaining saved slots take the lowest-id unassigned current item of
///    the same kind (stable creation order), minimized items included.
///    Tiled and floating slots go first, then minimized slots, each in
///    saved order.
/// 3. A remaining tiled or floating slot whose kind opens without a target
///    keeps its place under a placeholder and joins `to_open`; a singleton
///    viewer opens only if the Project has none. Every other remaining slot
///    is unfilled: its tab leaves its stack, empty stacks go, empty branches
///    collapse, surviving ratios renormalize.
/// 4. Unassigned visible items form one tiled **Additional windows** stack
///    appended on the right, or filling the Project when no saved tiled
///    geometry survived. Unassigned minimized items keep their stack and
///    restore placement.
///
/// Fails only when `def`, `current`, or `inventory` is itself invalid; the
/// live arrangement is never touched here.
pub fn reconcile(
    def: &LayoutDefinition,
    current: &Arrangement,
    inventory: &[ItemRef],
) -> Result<Reconciliation, LayoutError> {
    validate(&def.arrangement, &def.items)?;
    validate(current, inventory)?;
    let minimized_slot = minimized_ids(&def.arrangement);
    let minimized_now = minimized_ids(current);

    // Pass 1: exact id + kind.
    let current_kind: HashMap<LayoutItemId, ItemKind> =
        inventory.iter().map(|i| (i.id, i.kind)).collect();
    let mut saved_to_current: HashMap<LayoutItemId, LayoutItemId> = HashMap::new();
    let mut assigned: HashSet<LayoutItemId> = HashSet::new();
    let mut matched = Vec::new();
    for saved in &def.items {
        if current_kind.get(&saved.id) == Some(&saved.kind) {
            saved_to_current.insert(saved.id, saved.id);
            assigned.insert(saved.id);
            matched.push(saved.id);
        }
    }

    // Pass 2: same-kind substitutes in creation (ascending id) order. A
    // BTreeSet per kind gives "lowest unassigned id" deterministically.
    // Visible slots first, so a minimized slot never takes the item a
    // visible slot needed.
    let mut free: HashMap<ItemKind, BTreeSet<LayoutItemId>> = HashMap::new();
    for item in inventory {
        if !assigned.contains(&item.id) {
            free.entry(item.kind).or_default().insert(item.id);
        }
    }
    let mut substituted = Vec::new();
    for minimized in [false, true] {
        for saved in &def.items {
            if minimized_slot.contains(&saved.id) != minimized
                || saved_to_current.contains_key(&saved.id)
            {
                continue;
            }
            if let Some(id) = free.get_mut(&saved.kind).and_then(|pool| pool.pop_first()) {
                saved_to_current.insert(saved.id, id);
                assigned.insert(id);
                substituted.push(Substitution {
                    saved: saved.id,
                    current: id,
                    kind: saved.kind,
                });
            }
        }
    }

    // Pass 3: queue fresh opens or give the slot up. Placeholders count down
    // from u64::MAX past any current id; the inventory is bounded, so the
    // search always ends.
    let mut placeholders = (0..=LayoutItemId::MAX)
        .rev()
        .filter(|id| !current_kind.contains_key(id));
    let mut present: HashSet<ItemKind> = inventory.iter().map(|i| i.kind).collect();
    let mut to_open = Vec::new();
    let mut unfilled = Vec::new();
    for saved in &def.items {
        if saved_to_current.contains_key(&saved.id) {
            continue;
        }
        let opens = !minimized_slot.contains(&saved.id)
            && saved.kind.opens_without_target()
            && !(saved.kind.is_project_singleton() && present.contains(&saved.kind));
        if !opens {
            unfilled.push(*saved);
            continue;
        }
        let placeholder = placeholders
            .next()
            .expect("a bounded inventory leaves free ids");
        present.insert(saved.kind);
        saved_to_current.insert(saved.id, placeholder);
        to_open.push(OpenSlot {
            placeholder,
            saved: *saved,
        });
    }

    let (additional, kept_minimized): (Vec<ItemRef>, Vec<ItemRef>) = inventory
        .iter()
        .filter(|i| !assigned.contains(&i.id))
        .partition(|i| !minimized_now.contains(&i.id));

    // Pass 4: rebuild the saved windows with current ids and placeholders.
    let (mut windows, index_map) = remap_windows(&def.arrangement.windows, |id| {
        saved_to_current.get(&id).copied()
    });
    let mut tree = def
        .arrangement
        .tree
        .as_ref()
        .and_then(|t| prune_tree(t, &index_map));

    if !additional.is_empty() {
        let extra = windows.len();
        windows.push(WindowSpec::tiled(
            additional.iter().map(|i| i.id).collect(),
            0,
        ));
        let extra_leaf = TreeSpec::Leaf { window: extra };
        tree = Some(match tree.take() {
            None => extra_leaf,
            Some(existing) => TreeSpec::Split {
                dir: SplitAxis::H,
                ratios: vec![1.0 - ADDITIONAL_RATIO, ADDITIONAL_RATIO],
                children: vec![existing, extra_leaf],
            },
        });
    }

    // Kept-minimized items stay in their live minimized stacks, minus any
    // tab a slot took. Only minimized windows hold such items, so every
    // other live window empties and drops out.
    let keep: HashSet<LayoutItemId> = kept_minimized.iter().map(|i| i.id).collect();
    let (kept, _) = remap_windows(&current.windows, |id| keep.contains(&id).then_some(id));
    windows.extend(kept);

    let mut items = inventory.to_vec();
    items.extend(
        to_open
            .iter()
            .map(|o| ItemRef::new(o.placeholder, o.saved.kind)),
    );
    let arrangement = Arrangement { windows, tree };
    // The construction above is total by design; this guards the contract
    // that a Reconciliation is always applicable rather than trusting it.
    validate(&arrangement, &items)?;
    Ok(Reconciliation {
        arrangement,
        items,
        matched,
        substituted,
        to_open,
        opened: Vec::new(),
        additional,
        kept_minimized,
        unfilled,
    })
}

/// Ids in the minimized windows of `arrangement`.
fn minimized_ids(arrangement: &Arrangement) -> HashSet<LayoutItemId> {
    arrangement
        .windows
        .iter()
        .filter(|w| matches!(w.state, WindowState::Minimized { .. }))
        .flat_map(|w| w.tabs.iter().copied())
        .collect()
}

/// Rebuild `windows` through `map`: a tab mapping to `None` leaves its
/// stack and a stack left empty goes. A stack whose active tab vanished
/// keeps the same slot, clamped. Returns the windows and the old→new index
/// map that [`prune_tree`] needs.
fn remap_windows(
    windows: &[WindowSpec],
    map: impl Fn(LayoutItemId) -> Option<LayoutItemId>,
) -> (Vec<WindowSpec>, HashMap<usize, usize>) {
    let mut out: Vec<WindowSpec> = Vec::with_capacity(windows.len() + 1);
    let mut index_map: HashMap<usize, usize> = HashMap::new();
    for (wi, w) in windows.iter().enumerate() {
        let mut tabs = Vec::with_capacity(w.tabs.len());
        let mut active = None;
        for (ti, &id) in w.tabs.iter().enumerate() {
            let Some(to) = map(id) else {
                continue;
            };
            if ti == w.active {
                active = Some(tabs.len());
            }
            tabs.push(to);
        }
        if tabs.is_empty() {
            continue;
        }
        let active = active.unwrap_or_else(|| w.active.min(tabs.len() - 1));
        index_map.insert(wi, out.len());
        out.push(WindowSpec {
            tabs,
            active,
            state: w.state.clone(),
        });
    }
    (out, index_map)
}

/// Drop leaves whose window vanished, collapse single-child splits, and
/// renormalize surviving ratios. Mirrors `workspace::node_from_snap`.
fn prune_tree(node: &TreeSpec, index_map: &HashMap<usize, usize>) -> Option<TreeSpec> {
    match node {
        TreeSpec::Leaf { window } => index_map.get(window).map(|&w| TreeSpec::Leaf { window: w }),
        TreeSpec::Split {
            dir,
            ratios,
            children,
        } => {
            let mut kept = Vec::new();
            let mut kept_ratios = Vec::new();
            for (i, child) in children.iter().enumerate() {
                if let Some(c) = prune_tree(child, index_map) {
                    kept.push(c);
                    kept_ratios.push(ratios[i]);
                }
            }
            match kept.len() {
                0 => None,
                1 => kept.pop(),
                _ => Some(TreeSpec::Split {
                    dir: *dir,
                    ratios: normalize_ratios(&kept_ratios),
                    children: kept,
                }),
            }
        }
    }
}

/// Scale positive finite ratios to sum to 1.0; equal weights if they cannot.
fn normalize_ratios(ratios: &[f32]) -> Vec<f32> {
    let n = ratios.len();
    let sum: f32 = ratios.iter().sum();
    if n == 0 || !sum.is_finite() || sum <= 0.0 {
        return vec![1.0 / n.max(1) as f32; n];
    }
    ratios.iter().map(|r| r / sum).collect()
}

/// Modified comparison: true when `a` and `b` describe the same arrangement
/// for Modified purposes. Ignores active tabs and stacking order (focus
/// changes raise windows), compares tree shape and ratios, tab grouping and
/// order, each window's state, and floating/minimized geometry within
/// `GEOM_EPS`. Both sides must be normalized against the same area for the
/// geometry comparison to mean anything; callers compare captures taken
/// around a structural edit, not across a resize.
pub fn same_arrangement(a: &Arrangement, b: &Arrangement) -> bool {
    if a.windows.len() != b.windows.len() {
        return false;
    }
    // Windows are identified by their tab list (content identity + order).
    // A duplicate tab list cannot pass validation; fall back to equality.
    let by_tabs = |arr: &Arrangement| -> Option<HashMap<Vec<LayoutItemId>, WindowState>> {
        let mut m = HashMap::with_capacity(arr.windows.len());
        for w in &arr.windows {
            if m.insert(w.tabs.clone(), w.state.clone()).is_some() {
                return None;
            }
        }
        Some(m)
    };
    let (Some(wa), Some(wb)) = (by_tabs(a), by_tabs(b)) else {
        return a == b;
    };
    for (tabs, sa) in &wa {
        let Some(sb) = wb.get(tabs) else {
            return false;
        };
        if !same_state(sa, sb) {
            return false;
        }
    }
    match (&a.tree, &b.tree) {
        (None, None) => true,
        (Some(ta), Some(tb)) => same_tree(ta, &a.windows, tb, &b.windows),
        _ => false,
    }
}

fn same_state(a: &WindowState, b: &WindowState) -> bool {
    match (a, b) {
        (WindowState::Tiled, WindowState::Tiled) => true,
        (WindowState::Floating { rect: ra }, WindowState::Floating { rect: rb }) => {
            same_rect(ra, rb)
        }
        (
            WindowState::Minimized {
                from_tree: fa,
                rect: ra,
            },
            WindowState::Minimized {
                from_tree: fb,
                rect: rb,
            },
        ) => fa == fb && same_rect(ra, rb),
        _ => false,
    }
}

fn same_rect(a: &NormRect, b: &NormRect) -> bool {
    near(a.x, b.x) && near(a.y, b.y) && near(a.w, b.w) && near(a.h, b.h)
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() <= GEOM_EPS
}

fn same_tree(a: &TreeSpec, wa: &[WindowSpec], b: &TreeSpec, wb: &[WindowSpec]) -> bool {
    match (a, b) {
        (TreeSpec::Leaf { window: ia }, TreeSpec::Leaf { window: ib }) => {
            match (wa.get(*ia), wb.get(*ib)) {
                (Some(x), Some(y)) => x.tabs == y.tabs,
                _ => false,
            }
        }
        (
            TreeSpec::Split {
                dir: da,
                ratios: ra,
                children: ca,
            },
            TreeSpec::Split {
                dir: db,
                ratios: rb,
                children: cb,
            },
        ) => {
            da == db
                && ra.len() == rb.len()
                && ca.len() == cb.len()
                && ra.iter().zip(rb).all(|(x, y)| near(*x, *y))
                && ca.iter().zip(cb).all(|(x, y)| same_tree(x, wa, y, wb))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Size = Size::new(1000.0, 600.0);
    const MIN: Size = Size::new(240.0, 140.0);

    fn term(id: u64) -> ItemRef {
        ItemRef::new(id, ItemKind::Terminal)
    }
    fn chat(id: u64) -> ItemRef {
        ItemRef::new(id, ItemKind::Chat)
    }
    fn image(id: u64) -> ItemRef {
        ItemRef::new(id, ItemKind::Image)
    }

    fn leaf(w: usize) -> TreeSpec {
        TreeSpec::Leaf { window: w }
    }
    fn split(dir: SplitAxis, ratios: &[f32], children: Vec<TreeSpec>) -> TreeSpec {
        TreeSpec::Split {
            dir,
            ratios: ratios.to_vec(),
            children,
        }
    }

    /// A mixed definition: terminal 1 | [terminal 2, chat 3 stack] tiled,
    /// image 4 floating, terminal 5 minimized (from tree).
    fn mixed() -> LayoutDefinition {
        LayoutDefinition {
            id: "def-1".into(),
            name: "Mixed".into(),
            items: vec![term(1), term(2), chat(3), image(4), term(5)],
            arrangement: Arrangement {
                windows: vec![
                    WindowSpec::tiled(vec![1], 0),
                    WindowSpec::tiled(vec![2, 3], 1),
                    WindowSpec::floating(vec![4], 0, NormRect::new(0.5, 0.5, 0.4, 0.4)),
                    WindowSpec::minimized(vec![5], 0, true, NormRect::new(0.1, 0.1, 0.3, 0.3)),
                ],
                tree: Some(split(SplitAxis::H, &[0.6, 0.4], vec![leaf(0), leaf(1)])),
            },
        }
    }

    fn tabs_of(a: &Arrangement) -> Vec<Vec<u64>> {
        a.windows.iter().map(|w| w.tabs.clone()).collect()
    }

    // ── validation ───────────────────────────────────────────────────────

    #[test]
    fn mixed_definition_validates() {
        let d = mixed();
        assert_eq!(validate(&d.arrangement, &d.items), Ok(()));
    }

    #[test]
    fn validate_rejects_unknown_and_duplicate_and_missing_items() {
        let d = mixed();
        let mut a = d.arrangement.clone();
        a.windows[0].tabs = vec![99];
        assert_eq!(validate(&a, &d.items), Err(LayoutError::UnknownItem(99)));

        let mut a = d.arrangement.clone();
        a.windows[0].tabs = vec![1, 2];
        assert_eq!(validate(&a, &d.items), Err(LayoutError::DuplicateItem(2)));

        let mut a = d.arrangement.clone();
        a.windows[1].tabs = vec![2];
        a.windows[1].active = 0;
        assert_eq!(validate(&a, &d.items), Err(LayoutError::MissingItem(3)));

        let mut items = d.items.clone();
        items.push(term(1));
        assert_eq!(validate_items(&items), Err(LayoutError::DuplicateItem(1)));
    }

    #[test]
    fn validate_rejects_empty_stack_and_bad_active_tab() {
        let d = mixed();
        let mut a = d.arrangement.clone();
        a.windows[1].active = 2;
        assert_eq!(
            validate(&a, &d.items),
            Err(LayoutError::BadActiveTab {
                window: 1,
                active: 2
            })
        );

        let mut a = d.arrangement.clone();
        a.windows.push(WindowSpec::tiled(vec![], 0));
        let items = d.items.clone();
        assert_eq!(validate(&a, &items), Err(LayoutError::EmptyStack(4)));
    }

    #[test]
    fn validate_rejects_malformed_trees() {
        let d = mixed();
        // Leaf beyond the window list.
        let mut a = d.arrangement.clone();
        a.tree = Some(split(SplitAxis::H, &[0.5, 0.5], vec![leaf(0), leaf(7)]));
        assert_eq!(validate(&a, &d.items), Err(LayoutError::LeafOutOfRange(7)));
        // Leaf on a floating window.
        let mut a = d.arrangement.clone();
        a.tree = Some(split(SplitAxis::H, &[0.5, 0.5], vec![leaf(0), leaf(2)]));
        assert_eq!(validate(&a, &d.items), Err(LayoutError::LeafNotTiled(2)));
        // Same window twice.
        let mut a = d.arrangement.clone();
        a.tree = Some(split(
            SplitAxis::H,
            &[0.3, 0.3, 0.4],
            vec![leaf(0), leaf(1), leaf(0)],
        ));
        assert_eq!(validate(&a, &d.items), Err(LayoutError::DuplicateLeaf(0)));
        // Tiled window absent from the tree.
        let mut a = d.arrangement.clone();
        a.tree = Some(leaf(0));
        assert_eq!(
            validate(&a, &d.items),
            Err(LayoutError::TiledWindowNotInTree(1))
        );
        // One-child split and ratio-count mismatch.
        let mut a = d.arrangement.clone();
        a.tree = Some(split(
            SplitAxis::V,
            &[1.0],
            vec![split(SplitAxis::H, &[0.5, 0.5], vec![leaf(0), leaf(1)])],
        ));
        assert_eq!(validate(&a, &d.items), Err(LayoutError::MalformedSplit));
        let mut a = d.arrangement.clone();
        a.tree = Some(split(SplitAxis::H, &[0.5], vec![leaf(0), leaf(1)]));
        assert_eq!(validate(&a, &d.items), Err(LayoutError::MalformedSplit));
        // Tiled windows with no tree at all.
        let mut a = d.arrangement.clone();
        a.tree = None;
        assert_eq!(
            validate(&a, &d.items),
            Err(LayoutError::TiledWindowNotInTree(0))
        );
    }

    #[test]
    fn validate_rejects_non_finite_and_degenerate_geometry() {
        let d = mixed();
        let mut a = d.arrangement.clone();
        a.tree = Some(split(
            SplitAxis::H,
            &[f32::NAN, 0.5],
            vec![leaf(0), leaf(1)],
        ));
        assert_eq!(validate(&a, &d.items), Err(LayoutError::NonFiniteRatio));
        let mut a = d.arrangement.clone();
        a.tree = Some(split(SplitAxis::H, &[0.0, 1.0], vec![leaf(0), leaf(1)]));
        assert_eq!(validate(&a, &d.items), Err(LayoutError::NonFiniteRatio));
        let mut a = d.arrangement.clone();
        a.tree = Some(split(
            SplitAxis::H,
            &[f32::INFINITY, 1.0],
            vec![leaf(0), leaf(1)],
        ));
        assert_eq!(validate(&a, &d.items), Err(LayoutError::NonFiniteRatio));

        let mut a = d.arrangement.clone();
        a.windows[2].state = WindowState::Floating {
            rect: NormRect::new(f32::NAN, 0.0, 0.5, 0.5),
        };
        assert_eq!(validate(&a, &d.items), Err(LayoutError::NonFiniteRect(2)));
        let mut a = d.arrangement.clone();
        a.windows[3].state = WindowState::Minimized {
            from_tree: false,
            rect: NormRect::new(0.0, 0.0, 0.0, 0.5),
        };
        assert_eq!(validate(&a, &d.items), Err(LayoutError::DegenerateRect(3)));
    }

    #[test]
    fn validate_rejects_oversized_inventories_and_deep_trees() {
        let items: Vec<ItemRef> = (1..=(MAX_ITEMS as u64 + 1)).map(term).collect();
        assert_eq!(
            validate_items(&items),
            Err(LayoutError::TooManyItems(MAX_ITEMS + 1))
        );

        // A chain of nested two-child splits deeper than the bound.
        let n = MAX_TREE_DEPTH + 1;
        let items: Vec<ItemRef> = (1..=n as u64).map(term).collect();
        let windows: Vec<WindowSpec> = (0..n)
            .map(|i| WindowSpec::tiled(vec![i as u64 + 1], 0))
            .collect();
        let mut tree = leaf(n - 1);
        for i in (0..n - 1).rev() {
            tree = split(SplitAxis::H, &[0.5, 0.5], vec![leaf(i), tree]);
        }
        let a = Arrangement {
            windows,
            tree: Some(tree),
        };
        assert_eq!(validate(&a, &items), Err(LayoutError::TreeTooDeep));
    }

    #[test]
    fn serde_rejects_unknown_fields_and_round_trips() {
        let d = mixed();
        let json = serde_json::to_string(&d).unwrap();
        let back: LayoutDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);

        let bad = r#"{"tabs":[1],"active":0,"state":{"state":"Tiled"},"exec":"rm"}"#;
        assert!(serde_json::from_str::<WindowSpec>(bad).is_err());
        let bad_rect = r#"{"x":0,"y":0,"w":1,"h":1,"z":9}"#;
        assert!(serde_json::from_str::<NormRect>(bad_rect).is_err());
        let bad_tree = r#"{"kind":"Leaf","window":0,"cmd":"x"}"#;
        assert!(serde_json::from_str::<TreeSpec>(bad_tree).is_err());
    }

    #[test]
    fn name_rules_trim_and_bound() {
        assert_eq!(validate_name("  Coding  "), Ok("Coding".into()));
        assert_eq!(validate_name("   "), Err(LayoutError::EmptyName));
        let long = "x".repeat(MAX_NAME_CHARS + 1);
        assert_eq!(
            validate_name(&long),
            Err(LayoutError::NameTooLong(MAX_NAME_CHARS + 1))
        );
        assert_eq!(
            validate_name(&"é".repeat(MAX_NAME_CHARS))
                .unwrap()
                .chars()
                .count(),
            MAX_NAME_CHARS
        );
        assert!(names_collide("Coding", "coding "));
        assert!(!names_collide("Coding", "Coding 2"));
    }

    // ── geometry ─────────────────────────────────────────────────────────

    #[test]
    fn normalized_geometry_adapts_to_different_areas() {
        let local = LocalRect {
            x: 100.0,
            y: 60.0,
            w: 500.0,
            h: 300.0,
        };
        let n = NormRect::from_local(local, AREA);
        assert!((n.x - 0.1).abs() < 1e-6 && (n.w - 0.5).abs() < 1e-6);
        let same = n.to_local(AREA, MIN);
        assert!((same.x - 100.0).abs() < 1e-3 && (same.w - 500.0).abs() < 1e-3);
        let bigger = n.to_local(Size::new(2000.0, 1200.0), MIN);
        assert!((bigger.x - 200.0).abs() < 1e-3 && (bigger.w - 1000.0).abs() < 1e-3);
        assert!((bigger.h - 600.0).abs() < 1e-3);
    }

    #[test]
    fn denormalize_confines_and_enforces_size_floor() {
        // Off the right/bottom edge: pulled back inside.
        let r = NormRect::new(0.9, 0.9, 0.5, 0.5).to_local(AREA, MIN);
        assert!(r.x + r.w <= AREA.w + 1e-3 && r.y + r.h <= AREA.h + 1e-3);
        assert!(r.x >= 0.0 && r.y >= 0.0);
        // Tiny: grows to the floor.
        let r = NormRect::new(0.0, 0.0, 0.01, 0.01).to_local(AREA, MIN);
        assert!((r.w - MIN.w).abs() < 1e-3 && (r.h - MIN.h).abs() < 1e-3);
        // Larger than the area: capped at the area.
        let r = NormRect::new(-1.0, -1.0, 5.0, 5.0).to_local(AREA, MIN);
        assert!((r.w - AREA.w).abs() < 1e-3 && (r.h - AREA.h).abs() < 1e-3);
        assert_eq!((r.x, r.y), (0.0, 0.0));
        // An area smaller than the floor: the floor yields to the area.
        let r = NormRect::new(0.0, 0.0, 0.5, 0.5).to_local(Size::new(100.0, 50.0), MIN);
        assert!((r.w - 100.0).abs() < 1e-3 && (r.h - 50.0).abs() < 1e-3);
        // A zero area never divides by zero.
        let n = NormRect::from_local(
            LocalRect {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 10.0,
            },
            Size::new(0.0, 0.0),
        );
        assert!(n.is_finite());
    }

    // ── reconciliation ───────────────────────────────────────────────────

    const MIN_RECT: NormRect = NormRect::new(0.2, 0.2, 0.4, 0.4);

    /// A live arrangement for `inv`: every id not in `minimized` in one
    /// tiled stack (inventory order), every id in `minimized` its own
    /// minimized window.
    fn live(inv: &[ItemRef], minimized: &[u64]) -> Arrangement {
        let visible: Vec<u64> = inv
            .iter()
            .map(|i| i.id)
            .filter(|id| !minimized.contains(id))
            .collect();
        let mut windows = Vec::new();
        let mut tree = None;
        if !visible.is_empty() {
            windows.push(WindowSpec::tiled(visible, 0));
            tree = Some(leaf(0));
        }
        for &id in minimized {
            windows.push(WindowSpec::minimized(vec![id], 0, true, MIN_RECT));
        }
        Arrangement { windows, tree }
    }

    fn rec(d: &LayoutDefinition, inv: &[ItemRef]) -> Reconciliation {
        reconcile(d, &live(inv, &[]), inv).unwrap()
    }

    fn sub(saved: u64, current: u64, kind: ItemKind) -> Substitution {
        Substitution {
            saved,
            current,
            kind,
        }
    }

    fn def(
        items: Vec<ItemRef>,
        windows: Vec<WindowSpec>,
        tree: Option<TreeSpec>,
    ) -> LayoutDefinition {
        LayoutDefinition {
            id: "d".into(),
            name: "n".into(),
            items,
            arrangement: Arrangement { windows, tree },
        }
    }

    /// Every id the arrangement places, sorted.
    fn placed(a: &Arrangement) -> Vec<u64> {
        let mut v: Vec<u64> = a.windows.iter().flat_map(|w| w.tabs.clone()).collect();
        v.sort_unstable();
        v
    }

    #[test]
    fn exact_inventory_reconciles_unchanged() {
        let d = mixed();
        let r = reconcile(&d, &d.arrangement, &d.items).unwrap();
        assert!(r.is_exact());
        assert_eq!(r.arrangement, d.arrangement);
        assert_eq!(r.matched, vec![1, 2, 3, 4, 5]);
        assert!(r.to_open.is_empty() && r.kept_minimized.is_empty());
        assert_eq!(validate(&r.arrangement, &r.items), Ok(()));
    }

    #[test]
    fn target_bound_missing_slot_is_unfilled_and_its_branch_collapses() {
        // Image 4 has no stand-in and an image needs a target, so its slot
        // goes and the split collapses to the surviving stack.
        let d = def(
            vec![image(4), term(2), chat(3)],
            vec![
                WindowSpec::tiled(vec![4], 0),
                WindowSpec::tiled(vec![2, 3], 1),
            ],
            Some(split(SplitAxis::H, &[0.6, 0.4], vec![leaf(0), leaf(1)])),
        );
        let inv = vec![term(2), chat(3)];
        let r = rec(&d, &inv);
        assert_eq!(r.unfilled, vec![image(4)]);
        assert!(r.to_open.is_empty() && r.additional.is_empty());
        assert_eq!(tabs_of(&r.arrangement), vec![vec![2, 3]]);
        assert_eq!(r.arrangement.tree, Some(leaf(0)));
        assert_eq!(r.arrangement.windows[0].active, 1, "active tab survives");
        assert!(!r.is_exact(), "an unfilled slot is not clean");
        assert_eq!(validate(&r.arrangement, &inv), Ok(()));
    }

    #[test]
    fn same_kind_substitution_uses_creation_order_and_never_crosses_kinds() {
        let d = mixed();
        // Terminal 1 gone; terminals 9 and 7 are new. 7 is the older unassigned
        // terminal so it stands in for 1; 9 is extra. Chat 8 is a second chat
        // and must not stand in for a terminal.
        let inv = vec![
            term(9),
            term(2),
            chat(3),
            image(4),
            term(5),
            term(7),
            chat(8),
        ];
        let r = rec(&d, &inv);
        assert_eq!(r.substituted, vec![sub(1, 7, ItemKind::Terminal)]);
        assert!(r.unfilled.is_empty() && r.to_open.is_empty());
        assert_eq!(r.additional, vec![term(9), chat(8)], "inventory order");
        assert_eq!(
            tabs_of(&r.arrangement),
            vec![vec![7], vec![2, 3], vec![4], vec![5], vec![9, 8]]
        );
        let extra = &r.arrangement.windows[4];
        assert!(extra.is_tiled() && extra.active == 0);
        assert_eq!(
            r.arrangement.tree,
            Some(split(
                SplitAxis::H,
                &[1.0 - ADDITIONAL_RATIO, ADDITIONAL_RATIO],
                vec![
                    split(SplitAxis::H, &[0.6, 0.4], vec![leaf(0), leaf(1)]),
                    leaf(4)
                ]
            ))
        );
        assert!(!r.is_exact(), "extras went to Additional windows");
        assert_eq!(validate(&r.arrangement, &inv), Ok(()));
    }

    #[test]
    fn substitution_alone_is_still_clean() {
        // A reopened Project: its one fresh terminal never matches by id.
        let d = def(
            vec![term(1)],
            vec![WindowSpec::tiled(vec![1], 0)],
            Some(leaf(0)),
        );
        let r = rec(&d, &[term(40)]);
        assert_eq!(r.substituted, vec![sub(1, 40, ItemKind::Terminal)]);
        assert!(r.is_exact());
    }

    #[test]
    fn substitution_prefers_exact_matches_over_order() {
        // Saved [1, 2]; current [2, 3]: 2 matches exactly even though 1 is the
        // first saved item and 2 is the lowest free id at that moment.
        let d = def(
            vec![term(1), term(2)],
            vec![WindowSpec::tiled(vec![1], 0), WindowSpec::tiled(vec![2], 0)],
            Some(split(SplitAxis::V, &[0.5, 0.5], vec![leaf(0), leaf(1)])),
        );
        let r = rec(&d, &[term(2), term(3)]);
        assert_eq!(r.matched, vec![2]);
        assert_eq!(r.substituted, vec![sub(1, 3, ItemKind::Terminal)]);
        assert_eq!(tabs_of(&r.arrangement), vec![vec![3], vec![2]]);
    }

    #[test]
    fn minimized_current_item_substitutes_into_a_visible_slot() {
        let d = def(
            vec![term(1)],
            vec![WindowSpec::tiled(vec![1], 0)],
            Some(leaf(0)),
        );
        let inv = vec![term(9)];
        let r = reconcile(&d, &live(&inv, &[9]), &inv).unwrap();
        assert_eq!(r.substituted, vec![sub(1, 9, ItemKind::Terminal)]);
        assert!(r.kept_minimized.is_empty() && r.to_open.is_empty());
        assert_eq!(r.arrangement.windows, vec![WindowSpec::tiled(vec![9], 0)]);
        assert!(r.is_exact());
    }

    #[test]
    fn visible_slots_fill_before_minimized_slots() {
        // The minimized slot is listed first, but the one terminal goes to
        // the tiled slot; the minimized slot is never opened fresh.
        let d = def(
            vec![term(1), term(2)],
            vec![
                WindowSpec::minimized(vec![1], 0, true, MIN_RECT),
                WindowSpec::tiled(vec![2], 0),
            ],
            Some(leaf(1)),
        );
        let r = rec(&d, &[term(9)]);
        assert_eq!(r.substituted, vec![sub(2, 9, ItemKind::Terminal)]);
        assert_eq!(r.unfilled, vec![term(1)]);
        assert!(r.to_open.is_empty());
        assert_eq!(r.arrangement.windows, vec![WindowSpec::tiled(vec![9], 0)]);
        assert_eq!(r.arrangement.tree, Some(leaf(0)));
    }

    #[test]
    fn minimized_extras_keep_their_window_and_restore_placement() {
        let d = def(
            vec![term(1), chat(3)],
            vec![WindowSpec::tiled(vec![1], 0), WindowSpec::tiled(vec![3], 0)],
            Some(split(SplitAxis::H, &[0.5, 0.5], vec![leaf(0), leaf(1)])),
        );
        let inv = vec![term(1), term(9), term(7), chat(8)];
        let stack = WindowSpec::minimized(vec![7, 8], 1, false, NormRect::new(0.3, 0.1, 0.5, 0.6));
        let current = Arrangement {
            windows: vec![WindowSpec::tiled(vec![1, 9], 0), stack.clone()],
            tree: Some(leaf(0)),
        };
        let r = reconcile(&d, &current, &inv).unwrap();
        // Chat 8 leaves the minimized stack for the chat slot; terminal 7
        // stays minimized alone with the stack's placement (active clamped).
        assert_eq!(r.substituted, vec![sub(3, 8, ItemKind::Chat)]);
        assert_eq!(r.kept_minimized, vec![term(7)]);
        assert_eq!(r.additional, vec![term(9)], "visible extra");
        let kept = r
            .arrangement
            .windows
            .iter()
            .find(|w| w.tabs == vec![7])
            .unwrap();
        assert_eq!(kept.state, stack.state);
        assert_eq!(kept.active, 0);
        assert_eq!(validate(&r.arrangement, &inv), Ok(()));

        // With no chat slot the whole stack stays as it was, and an Apply that
        // only leaves minimized extras behind is clean.
        let d = def(
            vec![term(1)],
            vec![WindowSpec::tiled(vec![1], 0)],
            Some(leaf(0)),
        );
        let inv = vec![term(1), term(7), chat(8)];
        let current = Arrangement {
            windows: vec![WindowSpec::tiled(vec![1], 0), stack.clone()],
            tree: Some(leaf(0)),
        };
        let r = reconcile(&d, &current, &inv).unwrap();
        assert_eq!(r.kept_minimized, vec![term(7), chat(8)]);
        assert!(r.additional.is_empty());
        assert_eq!(
            r.arrangement.windows,
            vec![WindowSpec::tiled(vec![1], 0), stack]
        );
        assert!(r.is_exact());
    }

    #[test]
    fn openable_kinds_match_the_wm_open_paths() {
        use ItemKind::*;
        for k in [Terminal, Chat, Board, Plan, GitHistory, GitChanges] {
            assert!(k.opens_without_target(), "{k:?}");
        }
        for k in [Image, GitDiff, Project] {
            assert!(!k.opens_without_target(), "{k:?}");
        }
    }

    #[test]
    fn missing_visible_slots_of_openable_kinds_are_queued_in_place() {
        // An empty Project against `mixed`: terminals 1 and 2 and chat 3 are
        // tiled and open fresh; image 4 needs a target; terminal 5 is a
        // minimized slot. Nothing is current, so nothing substitutes.
        let d = mixed();
        let r = reconcile(&d, &Arrangement::default(), &[]).unwrap();
        let queued: Vec<ItemRef> = r.to_open.iter().map(|o| o.saved).collect();
        assert_eq!(queued, vec![term(1), term(2), chat(3)]);
        assert_eq!(r.unfilled, vec![image(4), term(5)]);
        // Placeholders sit exactly where the saved slots were.
        let p: Vec<u64> = r.to_open.iter().map(|o| o.placeholder).collect();
        assert_eq!(tabs_of(&r.arrangement), vec![vec![p[0]], vec![p[1], p[2]]]);
        assert_eq!(r.arrangement.windows[1].active, 1);
        assert_eq!(r.arrangement.tree, d.arrangement.tree);
        assert_eq!(validate(&r.arrangement, &r.items), Ok(()));
        assert!(!r.is_exact(), "pending opens are not filled yet");
    }

    #[test]
    fn placeholders_never_collide_with_current_ids() {
        let d = def(
            vec![chat(1)],
            vec![WindowSpec::tiled(vec![1], 0)],
            Some(leaf(0)),
        );
        let inv = vec![term(u64::MAX), term(u64::MAX - 1)];
        let r = rec(&d, &inv);
        assert_eq!(r.to_open.len(), 1);
        assert!(!inv.iter().any(|i| i.id == r.to_open[0].placeholder));
        assert_eq!(validate(&r.arrangement, &r.items), Ok(()));
    }

    #[test]
    fn a_floating_slot_opens_fresh_too() {
        let d = def(
            vec![chat(3)],
            vec![WindowSpec::floating(vec![3], 0, MIN_RECT)],
            None,
        );
        let r = rec(&d, &[]);
        assert_eq!(r.to_open.len(), 1);
        assert_eq!(
            r.arrangement.windows[0].state,
            WindowState::Floating { rect: MIN_RECT }
        );
    }

    #[test]
    fn singleton_viewers_open_at_most_once_and_never_beside_an_existing_one() {
        let two_chats = def(
            vec![chat(1), chat(2)],
            vec![WindowSpec::tiled(vec![1], 0), WindowSpec::tiled(vec![2], 0)],
            Some(split(SplitAxis::H, &[0.5, 0.5], vec![leaf(0), leaf(1)])),
        );
        let r = rec(&two_chats, &[]);
        let queued: Vec<ItemRef> = r.to_open.iter().map(|o| o.saved).collect();
        assert_eq!(queued, vec![chat(1)]);
        assert_eq!(r.unfilled, vec![chat(2)]);
        // A current chat fills the first slot; the second is not opened,
        // because the chat open path would surface the existing one.
        let r = rec(&two_chats, &[chat(9)]);
        assert_eq!(r.substituted, vec![sub(1, 9, ItemKind::Chat)]);
        assert!(r.to_open.is_empty());
        assert_eq!(r.unfilled, vec![chat(2)]);
        // Terminals are not singletons.
        let two_terms = def(
            vec![term(1), term(2)],
            vec![WindowSpec::tiled(vec![1], 0), WindowSpec::tiled(vec![2], 0)],
            Some(split(SplitAxis::H, &[0.5, 0.5], vec![leaf(0), leaf(1)])),
        );
        assert_eq!(rec(&two_terms, &[]).to_open.len(), 2);
    }

    #[test]
    fn with_opened_swaps_placeholders_and_omits_failed_opens() {
        // A reopened Project: one fresh terminal 40 against `mixed`.
        let d = mixed();
        let r = rec(&d, &[term(40)]);
        assert_eq!(r.substituted, vec![sub(1, 40, ItemKind::Terminal)]);
        let queued: Vec<ItemRef> = r.to_open.iter().map(|o| o.saved).collect();
        assert_eq!(queued, vec![term(2), chat(3)]);
        let (p_term, p_chat) = (r.to_open[0].placeholder, r.to_open[1].placeholder);

        // Both opens succeed: the saved shape is back, ids are the new ones.
        let done = r
            .with_opened(&[(p_term, term(41)), (p_chat, chat(42))])
            .unwrap();
        assert!(done.to_open.is_empty());
        assert_eq!(
            done.opened,
            vec![sub(2, 41, ItemKind::Terminal), sub(3, 42, ItemKind::Chat)]
        );
        assert_eq!(tabs_of(&done.arrangement), vec![vec![40], vec![41, 42]]);
        assert_eq!(done.arrangement.windows[1].active, 1);
        assert_eq!(done.arrangement.tree, d.arrangement.tree);
        assert_eq!(done.unfilled, vec![image(4), term(5)]);
        assert_eq!(placed(&done.arrangement), vec![40, 41, 42]);
        assert_eq!(validate(&done.arrangement, &done.items), Ok(()));

        // The chat fails to open: its tab leaves the stack; the summary says so.
        let part = r.with_opened(&[(p_term, term(41))]).unwrap();
        assert_eq!(part.opened, vec![sub(2, 41, ItemKind::Terminal)]);
        assert_eq!(part.unfilled, vec![image(4), term(5), chat(3)]);
        assert_eq!(tabs_of(&part.arrangement), vec![vec![40], vec![41]]);
        assert_eq!(part.arrangement.windows[1].active, 0);
        assert_eq!(validate(&part.arrangement, &part.items), Ok(()));

        // Everything fails: only the substituted terminal remains.
        let none = r.with_opened(&[]).unwrap();
        assert_eq!(tabs_of(&none.arrangement), vec![vec![40]]);
        assert_eq!(none.arrangement.tree, Some(leaf(0)));
    }

    #[test]
    fn with_opened_rejects_bad_reports() {
        let d = def(
            vec![term(1)],
            vec![WindowSpec::tiled(vec![1], 0)],
            Some(leaf(0)),
        );
        let r = rec(&d, &[chat(5)]);
        let p = r.to_open[0].placeholder;
        // Not a placeholder of this reconciliation.
        assert_eq!(
            r.with_opened(&[(77, term(8))]).unwrap_err(),
            LayoutError::UnknownItem(77)
        );
        // The new item collides with a current one.
        assert_eq!(
            r.with_opened(&[(p, term(5))]).unwrap_err(),
            LayoutError::DuplicateItem(5)
        );
        // The new item is the wrong kind.
        assert_eq!(
            r.with_opened(&[(p, chat(8))]).unwrap_err(),
            LayoutError::KindMismatch(8)
        );
    }

    #[test]
    fn filled_slots_make_a_clean_apply() {
        let d = def(
            vec![term(1), term(2)],
            vec![WindowSpec::tiled(vec![1], 0), WindowSpec::tiled(vec![2], 0)],
            Some(split(SplitAxis::H, &[0.5, 0.5], vec![leaf(0), leaf(1)])),
        );
        let r = rec(&d, &[term(40)]);
        let p = r.to_open[0].placeholder;
        let done = r.with_opened(&[(p, term(41))]).unwrap();
        assert!(
            done.is_exact(),
            "substituted + opened, nothing extra, nothing unfilled"
        );
        assert!(
            !r.with_opened(&[]).unwrap().is_exact(),
            "a failed open is unfilled"
        );
    }

    #[test]
    fn extras_fill_the_project_when_no_saved_tiles_survive() {
        let d = def(
            vec![image(4)],
            vec![WindowSpec::floating(vec![4], 0, MIN_RECT)],
            None,
        );
        let inv = vec![image(4), chat(21)];
        let r = rec(&d, &inv);
        assert_eq!(tabs_of(&r.arrangement), vec![vec![4], vec![21]]);
        assert_eq!(r.arrangement.tree, Some(leaf(1)));
        assert_eq!(validate(&r.arrangement, &inv), Ok(()));
    }

    #[test]
    fn nothing_current_and_nothing_openable_yields_an_empty_project() {
        let d = def(
            vec![image(4), term(5)],
            vec![
                WindowSpec::floating(vec![4], 0, MIN_RECT),
                WindowSpec::minimized(vec![5], 0, true, MIN_RECT),
            ],
            None,
        );
        let r = reconcile(&d, &Arrangement::default(), &[]).unwrap();
        assert_eq!(r.arrangement, Arrangement::default());
        assert_eq!(r.unfilled, vec![image(4), term(5)]);
        assert!(r.to_open.is_empty());
        assert_eq!(validate(&r.arrangement, &[]), Ok(()));
    }

    #[test]
    fn all_minimized_result_is_valid() {
        let d = def(
            vec![term(1), chat(2)],
            vec![
                WindowSpec::minimized(vec![1], 0, true, NormRect::new(0.0, 0.0, 0.5, 0.5)),
                WindowSpec::minimized(vec![2], 0, false, NormRect::new(0.2, 0.2, 0.5, 0.5)),
            ],
            None,
        );
        let r = reconcile(&d, &d.arrangement, &d.items).unwrap();
        assert!(r.is_exact());
        assert!(r.arrangement.tree.is_none());
        assert_eq!(validate(&r.arrangement, &d.items), Ok(()));
    }

    #[test]
    fn a_to_b_to_a_reuses_set_aside_windows_and_opens_nothing() {
        // A: terminal 1 | [terminal 2, chat 3], terminal 5 minimized.
        let a = def(
            vec![term(1), term(2), chat(3), term(5)],
            vec![
                WindowSpec::tiled(vec![1], 0),
                WindowSpec::tiled(vec![2, 3], 0),
                WindowSpec::minimized(vec![5], 0, true, MIN_RECT),
            ],
            Some(split(SplitAxis::H, &[0.6, 0.4], vec![leaf(0), leaf(1)])),
        );
        // B: one terminal over a board. Saved elsewhere, so its ids are foreign.
        let b = def(
            vec![term(100), ItemRef::new(101, ItemKind::Board)],
            vec![
                WindowSpec::tiled(vec![100], 0),
                WindowSpec::tiled(vec![101], 0),
            ],
            Some(split(SplitAxis::V, &[0.7, 0.3], vec![leaf(0), leaf(1)])),
        );
        let mut inv = a.items.clone();

        // A → B: terminal 1 substitutes; the board opens fresh; terminal 2
        // and chat 3 go to Additional; minimized terminal 5 stays put.
        let rb = reconcile(&b, &a.arrangement, &inv).unwrap();
        assert_eq!(rb.substituted, vec![sub(100, 1, ItemKind::Terminal)]);
        assert_eq!(rb.additional, vec![term(2), chat(3)]);
        assert_eq!(rb.kept_minimized, vec![term(5)]);
        assert_eq!(rb.to_open.len(), 1);
        let board = ItemRef::new(50, ItemKind::Board);
        let rb = rb
            .with_opened(&[(rb.to_open[0].placeholder, board)])
            .unwrap();
        inv.push(board);

        // B → A: every A slot comes back by exact id; nothing opens. The board
        // A never had goes to Additional windows.
        let ra = reconcile(&a, &rb.arrangement, &inv).unwrap();
        assert_eq!(ra.matched, vec![1, 2, 3, 5]);
        assert!(ra.to_open.is_empty() && ra.substituted.is_empty());
        assert_eq!(ra.additional, vec![board]);

        // A → B again reuses the set-aside board: the count stops growing.
        let rb2 = reconcile(&b, &ra.arrangement, &inv).unwrap();
        assert!(rb2.to_open.is_empty());
        assert_eq!(
            rb2.substituted,
            vec![
                sub(100, 1, ItemKind::Terminal),
                sub(101, 50, ItemKind::Board)
            ]
        );

        // Over A's own inventory, A → B → A opens nothing at all.
        let rb = reconcile(&b, &a.arrangement, &a.items).unwrap();
        let rb = rb.with_opened(&[]).unwrap();
        let back = reconcile(&a, &rb.arrangement, &a.items).unwrap();
        assert!(back.is_exact() && back.to_open.is_empty());
        assert_eq!(back.arrangement, a.arrangement);
    }

    #[test]
    fn reconcile_is_deterministic_and_total() {
        let d = mixed();
        let inv = vec![term(9), chat(3), term(7), image(4), image(6), chat(8)];
        let cur = live(&inv, &[6, 8]);
        let a = reconcile(&d, &cur, &inv).unwrap();
        let b = reconcile(&d, &cur, &inv).unwrap();
        assert_eq!(a, b);
        // Every current item exactly once, plus one slot per pending open.
        let mut want: Vec<u64> = inv.iter().map(|i| i.id).collect();
        want.extend(a.to_open.iter().map(|o| o.placeholder));
        want.sort_unstable();
        assert_eq!(placed(&a.arrangement), want);
        assert_eq!(validate(&a.arrangement, &a.items), Ok(()));
    }

    #[test]
    fn reconcile_rejects_invalid_definition_or_inventory() {
        let mut d = mixed();
        d.arrangement.windows[0].tabs = vec![1, 1];
        assert_eq!(
            reconcile(&d, &Arrangement::default(), &[]).unwrap_err(),
            LayoutError::DuplicateItem(1)
        );
        let d = mixed();
        assert_eq!(
            reconcile(&d, &Arrangement::default(), &[term(1), term(1)]).unwrap_err(),
            LayoutError::DuplicateItem(1)
        );
        // The live arrangement must place exactly the inventory.
        assert_eq!(
            reconcile(&d, &Arrangement::default(), &[term(1)]).unwrap_err(),
            LayoutError::MissingItem(1)
        );
    }

    // ── Modified comparison ──────────────────────────────────────────────

    #[test]
    fn same_arrangement_ignores_active_tab_and_stacking_order() {
        let base = mixed().arrangement;
        let mut b = base.clone();
        b.windows[1].active = 0;
        assert!(
            same_arrangement(&base, &b),
            "active tab is not a modification"
        );
        // Raise the floating window above the minimized one (focus changes z).
        let mut b = base.clone();
        b.windows.swap(2, 3);
        assert!(
            same_arrangement(&base, &b),
            "stacking order is not a modification"
        );
        // Reorder tiled windows in the list without changing the tree meaning.
        let mut b = base.clone();
        b.windows.swap(0, 1);
        b.tree = Some(split(SplitAxis::H, &[0.6, 0.4], vec![leaf(1), leaf(0)]));
        assert!(
            same_arrangement(&base, &b),
            "window list order is not a modification"
        );
    }

    #[test]
    fn same_arrangement_detects_structural_edits() {
        let base = mixed().arrangement;
        // Divider moved.
        let mut b = base.clone();
        b.tree = Some(split(SplitAxis::H, &[0.5, 0.5], vec![leaf(0), leaf(1)]));
        assert!(!same_arrangement(&base, &b));
        // Tab reordered inside a stack.
        let mut b = base.clone();
        b.windows[1].tabs = vec![3, 2];
        b.windows[1].active = 0;
        assert!(!same_arrangement(&base, &b));
        // Floating window moved.
        let mut b = base.clone();
        b.windows[2].state = WindowState::Floating {
            rect: NormRect::new(0.3, 0.5, 0.4, 0.4),
        };
        assert!(!same_arrangement(&base, &b));
        // Minimized window restored to floating.
        let mut b = base.clone();
        b.windows[3].state = WindowState::Floating {
            rect: NormRect::new(0.1, 0.1, 0.3, 0.3),
        };
        assert!(!same_arrangement(&base, &b));
        // Split direction changed.
        let mut b = base.clone();
        b.tree = Some(split(SplitAxis::V, &[0.6, 0.4], vec![leaf(0), leaf(1)]));
        assert!(!same_arrangement(&base, &b));
        // Content added.
        let mut b = base.clone();
        b.windows.push(WindowSpec::floating(
            vec![6],
            0,
            NormRect::new(0.0, 0.0, 0.5, 0.5),
        ));
        assert!(!same_arrangement(&base, &b));
    }

    #[test]
    fn same_arrangement_tolerates_float_noise() {
        let base = mixed().arrangement;
        let mut b = base.clone();
        b.tree = Some(split(
            SplitAxis::H,
            &[0.6 + 1e-5, 0.4 - 1e-5],
            vec![leaf(0), leaf(1)],
        ));
        b.windows[2].state = WindowState::Floating {
            rect: NormRect::new(0.5 + 1e-5, 0.5, 0.4, 0.4),
        };
        assert!(same_arrangement(&base, &b));
    }
}
