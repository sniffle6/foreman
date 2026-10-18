//! Live layout Apply and Revert on a Project's child `WindowManager`, plus
//! the event-driven Modified flag. The model (`project_layout`) decides what
//! the arrangement is; this module moves the existing `Tab`s into it.
//! Spec: `docs/superpowers/specs/2026-10-16-project-layouts-design.md`
//! (Applying saved layouts, Revert, Save behavior). Feature doc:
//! `docs/project-layouts.md`.
//!
//! Content is only ever moved. Fresh items come from the Project's own open
//! paths (`spawn_shell_tab`, `view_tab`) before the rearrangement, and
//! nothing is closed: every `Tab` taken out of a window goes back into one.

use super::*;
use crate::project_layout::{
    self as pl, Arrangement, ItemRef, LayoutDefinition, LayoutItemId, LayoutSelection, LocalRect,
    NormRect, Reconciliation, SplitAxis, TreeSpec, WindowSpec, WindowState,
};
use std::collections::{HashMap, HashSet};

/// A layout change waiting for the manager's next frame (the deferred
/// mutation boundary). The newest request replaces an unprocessed one.
#[cfg_attr(not(test), allow(dead_code))] // built by the layout picker wave
pub enum LayoutRequest {
    /// Apply `def`. `select: true` is a saved layout and selects it;
    /// `false` is a Create candidate and leaves the picker on Custom.
    Apply { def: LayoutDefinition, select: bool },
    /// Restore the arrangement from before the last Apply.
    Revert,
}

/// A fresh item that failed to open. Its slot was omitted.
#[derive(Clone, Debug, PartialEq)]
pub struct FailedOpen {
    pub saved: ItemRef,
    pub error: String,
}

/// What an Apply or Revert did, for the post-Apply summary.
#[derive(Clone, Debug)]
#[cfg_attr(not(test), allow(dead_code))] // read by the layout picker wave
pub struct LayoutApplied {
    /// The reconciliation that was applied, with fresh items resolved
    /// (`opened`) and failed opens moved to `unfilled`.
    pub reconciliation: Reconciliation,
    pub failed_opens: Vec<FailedOpen>,
}

#[cfg_attr(not(test), allow(dead_code))] // read by the layout picker wave
impl LayoutApplied {
    /// Clean Apply: every slot filled and nothing in Additional windows.
    pub fn is_exact(&self) -> bool {
        self.reconciliation.is_exact()
    }
}

/// Why an Apply or Revert did nothing. The live arrangement is intact.
#[derive(Clone, Debug, PartialEq)]
pub enum LayoutApplyError {
    /// Layouts arrange a Project's windows, not the desktop.
    NotAProject,
    /// The Project has not been laid out yet, so geometry has no area.
    NotLaidOut,
    /// A close confirmation is open; its target must not move under it.
    Busy,
    /// A window holds Content that never takes part in a layout.
    UnsupportedContent,
    Invalid(pl::LayoutError),
    NoRevertPoint,
    /// A structural edit since the Apply replaced what Revert would restore.
    RevertInvalidated,
}

impl std::fmt::Display for LayoutApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAProject => write!(f, "Open or select a Project to use layouts."),
            Self::NotLaidOut => write!(f, "The Project is not on screen yet. Try again."),
            Self::Busy => write!(f, "Answer the open dialog first."),
            Self::UnsupportedContent => {
                write!(f, "This Project holds a window that layouts cannot move.")
            }
            Self::Invalid(e) => write!(f, "The layout is invalid: {e}."),
            Self::NoRevertPoint => write!(f, "There is no layout change to revert."),
            Self::RevertInvalidated => {
                write!(f, "Windows changed since the layout was applied.")
            }
        }
    }
}

/// Whether **Revert Layout** can run, for the picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))] // read by the layout picker wave
pub enum RevertStatus {
    /// No Apply in this session (or the last Revert consumed it).
    Unavailable,
    Ready,
    /// Disabled; [`LayoutApplyError::RevertInvalidated`] explains why.
    Invalidated,
}

/// The one-level revert point: identities and geometry, never Content.
struct RevertPoint {
    items: Vec<ItemRef>,
    arrangement: Arrangement,
    selection: LayoutSelection,
}

#[derive(Default)]
enum Revert {
    #[default]
    None,
    Ready(RevertPoint),
    Invalidated,
}

/// Per-manager runtime state. Never persisted: Revert does not survive
/// restart, and the edit watch re-measures on its first frame.
#[derive(Default)]
pub(super) struct LayoutRuntime {
    pending: Option<LayoutRequest>,
    outcome: Option<Result<LayoutApplied, LayoutApplyError>>,
    revert: Revert,
    /// The arrangement as of the last check, and the area it was measured
    /// in. The edit watch compares against it.
    baseline: Option<(egui::Vec2, Arrangement)>,
    /// Set by `mark_workspace_dirty`: something may have changed structure.
    pub(super) probe: bool,
    /// Test seam: make every fresh open fail, as a shell that cannot spawn.
    #[cfg(test)]
    fail_opens: bool,
}

/// A `Tab` out of its window for the duration of a rearrangement.
struct Held {
    tab: Tab,
    /// The window it came from (or its fresh id), so a window can keep its id.
    from: WinId,
    /// The floating rect it restores to when untiled.
    float: Option<egui::Rect>,
}

impl WindowManager {
    /// Queue a layout change for the next frame of this Project.
    #[cfg_attr(not(test), allow(dead_code))] // called by the layout picker wave
    pub fn request_layout(&mut self, request: LayoutRequest) {
        self.layout_rt.pending = Some(request);
    }

    /// The result of the last processed request, once.
    #[cfg_attr(not(test), allow(dead_code))] // read by the layout picker wave
    pub fn take_layout_outcome(&mut self) -> Option<Result<LayoutApplied, LayoutApplyError>> {
        self.layout_rt.outcome.take()
    }

    #[cfg_attr(not(test), allow(dead_code))] // read by the layout picker wave
    pub fn layout_revert_status(&self) -> RevertStatus {
        match self.layout_rt.revert {
            Revert::None => RevertStatus::Unavailable,
            Revert::Ready(_) => RevertStatus::Ready,
            Revert::Invalidated => RevertStatus::Invalidated,
        }
    }

    /// Save succeeded for definition `id`: select it and clear Modified.
    #[cfg_attr(not(test), allow(dead_code))] // called by the layout picker wave
    pub fn mark_layout_saved(&mut self, id: String) {
        self.set_layout_selection(LayoutSelection {
            selected: Some(id),
            modified: false,
        });
    }

    /// Run the queued request, if any. Called from `show` once the area is
    /// known and before anything renders.
    pub(super) fn drain_layout_request(&mut self, ctx: &egui::Context) {
        let Some(request) = self.layout_rt.pending.take() else {
            return;
        };
        let outcome = match request {
            LayoutRequest::Apply { def, select } => self.apply_layout(&def, select, ctx),
            LayoutRequest::Revert => self.revert_layout(),
        };
        self.layout_rt.outcome = Some(outcome);
    }

    /// Apply `def` to this Project now. Reconcile first; open fresh items
    /// for fillable missing slots; then move every `Tab` into the result.
    /// On `Err` nothing moved. Nothing opened either, except on an internal
    /// failure after the opens, which parks them in floating windows rather
    /// than closing them.
    pub fn apply_layout(
        &mut self,
        def: &LayoutDefinition,
        select: bool,
        ctx: &egui::Context,
    ) -> Result<LayoutApplied, LayoutApplyError> {
        let area = self.layout_area()?;
        let (items, current) = self.capture_arrangement(area);
        let rec = pl::reconcile(def, &current, &items).map_err(LayoutApplyError::Invalid)?;
        let before = RevertPoint {
            items,
            arrangement: current,
            selection: self.layout.clone(),
        };

        // Open fresh items first, so the rearrangement only moves Content.
        let mut opened = Vec::new();
        let mut fresh = Vec::new();
        let mut failed_opens = Vec::new();
        for slot in &rec.to_open {
            match self.open_fresh_tab(slot.saved.kind, ctx) {
                Ok((win, tab)) => {
                    opened.push((
                        slot.placeholder,
                        ItemRef::new(tab.layout_id(), slot.saved.kind),
                    ));
                    fresh.push((win, tab));
                }
                Err(error) => failed_opens.push(FailedOpen {
                    saved: slot.saved,
                    error,
                }),
            }
        }
        let rec = match rec.with_opened(&opened) {
            Ok(rec) => rec,
            Err(e) => {
                self.park_fresh(fresh);
                return Err(LayoutApplyError::Invalid(e));
            }
        };
        self.rearrange(&rec.arrangement, fresh, area)?;

        let selection = if select {
            LayoutSelection {
                selected: Some(def.id.clone()),
                modified: !rec.is_exact(),
            }
        } else {
            LayoutSelection::default()
        };
        self.set_layout_selection(selection);
        self.layout_rt.revert = Revert::Ready(before);
        self.rebaseline_layout();
        Ok(LayoutApplied {
            reconciliation: rec,
            failed_opens,
        })
    }

    /// Restore the arrangement from before the last Apply, with its
    /// selection and Modified state. Content is not rewound. Items the
    /// Apply opened stay open, in Additional windows. One level only.
    pub fn revert_layout(&mut self) -> Result<LayoutApplied, LayoutApplyError> {
        let area = self.layout_area()?;
        let point = match std::mem::take(&mut self.layout_rt.revert) {
            Revert::Ready(point) => point,
            Revert::None => return Err(LayoutApplyError::NoRevertPoint),
            Revert::Invalidated => {
                self.layout_rt.revert = Revert::Invalidated;
                return Err(LayoutApplyError::RevertInvalidated);
            }
        };
        let (items, current) = self.capture_arrangement(area);
        let def = LayoutDefinition {
            id: String::new(),
            name: String::new(),
            items: point.items.clone(),
            arrangement: point.arrangement.clone(),
        };
        // Any inventory change invalidated the point, so every saved item
        // matches by id and the Apply's fresh items land in Additional
        // windows. A slot that would open anyway is dropped: Revert opens
        // nothing.
        let rec = pl::reconcile(&def, &current, &items)
            .and_then(|rec| rec.with_opened(&[]))
            .map_err(LayoutApplyError::Invalid);
        let rec = match rec {
            Ok(rec) => rec,
            Err(e) => {
                self.layout_rt.revert = Revert::Ready(point);
                return Err(e);
            }
        };
        if let Err(e) = self.rearrange(&rec.arrangement, Vec::new(), area) {
            self.layout_rt.revert = Revert::Ready(point);
            return Err(e);
        }
        self.set_layout_selection(point.selection);
        self.rebaseline_layout();
        Ok(LayoutApplied {
            reconciliation: rec,
            failed_opens: Vec::new(),
        })
    }

    /// Preconditions shared by Apply and Revert; returns the area.
    fn layout_area(&self) -> Result<egui::Vec2, LayoutApplyError> {
        if self.desktop {
            return Err(LayoutApplyError::NotAProject);
        }
        if self.app_modal || self.pending_close.is_some() {
            return Err(LayoutApplyError::Busy);
        }
        if self
            .windows
            .iter()
            .flat_map(|w| w.tabs.iter())
            .any(|t| t.content.layout_kind().is_none())
        {
            return Err(LayoutApplyError::UnsupportedContent);
        }
        let area = self.last_area;
        if area.x < 1.0 || area.y < 1.0 {
            return Err(LayoutApplyError::NotLaidOut);
        }
        Ok(area)
    }

    /// Open one fresh item of `kind` through the Project's own open paths:
    /// the default shell in the Project directory (no command, no agent),
    /// or a view built from Project state. Returns the id it was given.
    fn open_fresh_tab(
        &mut self,
        kind: pl::ItemKind,
        ctx: &egui::Context,
    ) -> Result<(WinId, Tab), String> {
        #[cfg(test)]
        if self.layout_rt.fail_opens {
            return Err("test: open refused".into());
        }
        if kind == pl::ItemKind::Terminal {
            let shell = crate::config::live(ctx).default_shell.to_shell();
            return self
                .spawn_shell_tab(shell, ctx)
                .map_err(|e| format!("terminal failed to start: {e}"));
        }
        let tab = self
            .view_tab(kind)
            .ok_or_else(|| format!("{kind:?} cannot open without a target"))?;
        let id = self.next;
        self.next += 1;
        Ok((id, tab))
    }

    /// Fallback for an internal failure after items opened: give each its
    /// own floating window rather than dropping (and so closing) it.
    fn park_fresh(&mut self, fresh: Vec<(WinId, Tab)>) {
        for (id, tab) in fresh {
            self.z += 1;
            let rect = self.cascade_rect(egui::vec2(580.0, 380.0));
            let focused = self.focused;
            self.push_win(id, tab, rect);
            self.focused = focused;
        }
        self.mark_workspace_dirty();
    }

    /// This manager's windows as layout data, normalized against `area`,
    /// with the inventory it places. Zoom is ignored: a zoomed floating
    /// window reports the rect it un-zooms to.
    pub(super) fn capture_arrangement(&self, area: egui::Vec2) -> (Vec<ItemRef>, Arrangement) {
        let items = self.layout_inventory();
        let size = pl::Size::new(area.x, area.y);
        let norm = |r: egui::Rect| {
            NormRect::from_local(
                LocalRect {
                    x: r.min.x,
                    y: r.min.y,
                    w: r.width().max(1.0),
                    h: r.height().max(1.0),
                },
                size,
            )
        };
        let mut ordered: Vec<&Win> = self.windows.iter().collect();
        ordered.sort_by_key(|w| w.z);
        let mut index: HashMap<WinId, usize> = HashMap::new();
        let mut windows = Vec::with_capacity(ordered.len());
        for w in ordered {
            let mut tabs = Vec::with_capacity(w.tabs.len());
            let mut active = 0;
            for (i, t) in w.tabs.iter().enumerate() {
                if t.content.layout_kind().is_none() {
                    continue;
                }
                if i == w.active {
                    active = tabs.len();
                }
                tabs.push(t.layout_id());
            }
            if tabs.is_empty() {
                continue;
            }
            let state = if w.minimized {
                WindowState::Minimized {
                    from_tree: w.min_from_tree,
                    rect: norm(w.rect),
                }
            } else if self.tree.contains(w.id) {
                index.insert(w.id, windows.len());
                WindowState::Tiled
            } else {
                let r = match (self.zoomed == Some(w.id), w.prev) {
                    (true, Some(prev)) => prev,
                    _ => w.rect,
                };
                WindowState::Floating { rect: norm(r) }
            };
            windows.push(WindowSpec {
                tabs,
                active,
                state,
            });
        }
        let tree = self.tree.root.as_ref().and_then(|n| tree_spec(n, &index));
        (items, Arrangement { windows, tree })
    }

    /// Move every live `Tab` (and the `fresh` ones) into `arrangement`.
    /// Checks first that it places exactly the tabs that exist; on `Err`
    /// nothing changed and `fresh` was parked in floating windows.
    fn rearrange(
        &mut self,
        arrangement: &Arrangement,
        fresh: Vec<(WinId, Tab)>,
        area: egui::Vec2,
    ) -> Result<(), LayoutApplyError> {
        let live: HashSet<LayoutItemId> = self
            .windows
            .iter()
            .flat_map(|w| w.tabs.iter())
            .chain(fresh.iter().map(|(_, t)| t))
            .map(|t| t.layout_id())
            .collect();
        let placed: Vec<LayoutItemId> = arrangement
            .windows
            .iter()
            .flat_map(|w| w.tabs.iter().copied())
            .collect();
        let placed_set: HashSet<LayoutItemId> = placed.iter().copied().collect();
        let mismatch = if placed.len() != placed_set.len() {
            let mut seen = HashSet::new();
            placed
                .iter()
                .find(|id| !seen.insert(**id))
                .map(|id| pl::LayoutError::DuplicateItem(*id))
        } else if let Some(id) = placed.iter().find(|id| !live.contains(id)) {
            Some(pl::LayoutError::UnknownItem(*id))
        } else {
            live.iter()
                .find(|id| !placed_set.contains(id))
                .map(|id| pl::LayoutError::MissingItem(*id))
        };
        if let Some(e) = mismatch {
            self.park_fresh(fresh);
            return Err(LayoutApplyError::Invalid(e));
        }

        // Focused Content, by identity: Win ids may be reassigned below.
        let focus_item = self
            .focused
            .and_then(|id| self.windows.iter().find(|w| w.id == id))
            .map(|w| w.tabs[w.active].layout_id());
        // Exit child zoom first so a zoomed float gives back its own rect.
        self.unzoom();

        let mut pool: HashMap<LayoutItemId, Held> = HashMap::new();
        for w in std::mem::take(&mut self.windows) {
            let tiled = self.tree.contains(w.id);
            let float = if tiled || (w.minimized && w.min_from_tree) {
                w.prev
            } else {
                Some(w.rect)
            };
            for tab in w.tabs {
                pool.insert(
                    tab.layout_id(),
                    Held {
                        tab,
                        from: w.id,
                        float,
                    },
                );
            }
        }
        for (id, tab) in fresh {
            pool.insert(
                tab.layout_id(),
                Held {
                    tab,
                    from: id,
                    float: None,
                },
            );
        }
        self.tree = Default::default();

        let size = pl::Size::new(area.x, area.y);
        let min = pl::Size::new(MIN_W, MIN_H);
        let to_rect = |r: &NormRect| {
            let l = r.to_local(size, min);
            egui::Rect::from_min_size(egui::pos2(l.x, l.y), egui::vec2(l.w, l.h))
        };
        let mut used: HashSet<WinId> = HashSet::new();
        let mut ids: Vec<WinId> = Vec::with_capacity(arrangement.windows.len());
        for spec in &arrangement.windows {
            let mut tabs = Vec::with_capacity(spec.tabs.len());
            let mut origins = Vec::with_capacity(spec.tabs.len());
            for id in &spec.tabs {
                let held = pool.remove(id).expect("placement checked above");
                origins.push((held.from, held.float));
                tabs.push(held.tab);
            }
            let active = spec.active.min(tabs.len() - 1);
            let float = origins[active].1;
            // Keep a window id where one of its tabs came from, so the
            // panel and focus see the same window where they can.
            let id = std::iter::once(origins[active].0)
                .chain(origins.iter().map(|o| o.0))
                .find(|id| !used.contains(id))
                .unwrap_or_else(|| {
                    let id = self.next;
                    self.next += 1;
                    id
                });
            used.insert(id);
            self.z += 1;
            let (rect, minimized, min_from_tree, prev) = match &spec.state {
                WindowState::Tiled => (float.unwrap_or(egui::Rect::NOTHING), false, false, float),
                WindowState::Floating { rect } => (to_rect(rect), false, false, None),
                WindowState::Minimized { from_tree, rect } => (
                    to_rect(rect),
                    true,
                    *from_tree,
                    if *from_tree { float } else { None },
                ),
            };
            self.windows.push(Win {
                id,
                tabs,
                active,
                rect,
                z: self.z,
                minimized,
                min_from_tree,
                prev,
            });
            ids.push(id);
        }
        debug_assert!(pool.is_empty(), "placement checked above");

        if let Some(spec) = &arrangement.tree {
            self.tree.root = Some(tree_node(spec, &ids));
        }
        let local = egui::Rect::from_min_size(egui::Pos2::ZERO, area);
        for (id, r) in self.tree.layout(local, SNAP_GAP) {
            if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                w.rect = r;
                if w.prev.is_none() {
                    w.prev = Some(r);
                }
            }
        }

        // Focus stays on the same Content while it is visible; otherwise the
        // first visible window in arrangement order; none if all minimized.
        let visible = |w: &Win| !w.minimized;
        let keep = focus_item.and_then(|item| {
            self.windows
                .iter()
                .find(|w| visible(w) && w.tabs[w.active].layout_id() == item)
                .map(|w| w.id)
        });
        let first = self.tree.leaves().first().copied().or_else(|| {
            self.windows
                .iter()
                .find(|w| visible(w) && !self.tree.contains(w.id))
                .map(|w| w.id)
        });
        self.focused = keep.or(first);
        self.last_focused = None;
        self.renaming = None;
        self.drag_from_tree = None;
        self.mark_workspace_dirty();
        Ok(())
    }

    /// Re-measure the edit watch's baseline without treating the change as
    /// an edit (first frame, a resize, or right after Apply/Revert).
    fn rebaseline_layout(&mut self) {
        let area = self.last_area;
        let (_, now) = self.capture_arrangement(area);
        self.layout_rt.baseline = Some((area, now));
        self.layout_rt.probe = false;
    }

    /// The Modified event source. After a frame in which anything marked
    /// the workspace dirty, compare the arrangement with the one before it
    /// (`same_arrangement`): a difference is a structural edit. Focus,
    /// active tabs, titles, viewer navigation, and zoom do not count, and a
    /// resize only re-measures. Never compares against a definition.
    pub(super) fn watch_layout_edits(&mut self) {
        if self.desktop {
            return;
        }
        let area = self.last_area;
        let same_area = self
            .layout_rt
            .baseline
            .as_ref()
            .is_some_and(|(a, _)| *a == area);
        if !same_area {
            self.rebaseline_layout();
            return;
        }
        if !std::mem::take(&mut self.layout_rt.probe) {
            return;
        }
        let (_, now) = self.capture_arrangement(area);
        let edited = self
            .layout_rt
            .baseline
            .as_ref()
            .is_some_and(|(_, before)| !pl::same_arrangement(before, &now));
        self.layout_rt.baseline = Some((area, now));
        if edited {
            self.note_structural_edit();
        }
    }

    /// A structural edit: set Modified on a selected layout and retire the
    /// revert point.
    fn note_structural_edit(&mut self) {
        if matches!(self.layout_rt.revert, Revert::Ready(_)) {
            self.layout_rt.revert = Revert::Invalidated;
        }
        if self.layout.selected.is_some() && !self.layout.modified {
            let selected = self.layout.selected.clone();
            self.set_layout_selection(LayoutSelection {
                selected,
                modified: true,
            });
        }
    }
}

/// The live tree as a `TreeSpec` over window indices. Leaves without an
/// index (no layout tabs) drop out and single-child splits collapse.
fn tree_spec(n: &crate::layout::Node, index: &HashMap<WinId, usize>) -> Option<TreeSpec> {
    use crate::layout::{Node, SplitDir};
    match n {
        Node::Leaf(id) => index.get(id).map(|&window| TreeSpec::Leaf { window }),
        Node::Split {
            dir,
            ratios,
            children,
        } => {
            let mut kept = Vec::with_capacity(children.len());
            let mut kept_ratios = Vec::with_capacity(children.len());
            for (c, r) in children.iter().zip(ratios) {
                if let Some(c) = tree_spec(c, index) {
                    kept.push(c);
                    kept_ratios.push(*r);
                }
            }
            match kept.len() {
                0 => None,
                1 => kept.pop(),
                _ => Some(TreeSpec::Split {
                    dir: match dir {
                        SplitDir::H => SplitAxis::H,
                        SplitDir::V => SplitAxis::V,
                    },
                    ratios: kept_ratios,
                    children: kept,
                }),
            }
        }
    }
}

/// A validated `TreeSpec` as a live tree node: leaves map through `ids`,
/// ratios renormalize to 1, and a same-direction child splices into its
/// parent (the shape `LayoutTree::insert_split` keeps).
fn tree_node(spec: &TreeSpec, ids: &[WinId]) -> crate::layout::Node {
    use crate::layout::{Node, SplitDir};
    match spec {
        TreeSpec::Leaf { window } => Node::Leaf(ids[*window]),
        TreeSpec::Split {
            dir,
            ratios,
            children,
        } => {
            let dir = match dir {
                SplitAxis::H => SplitDir::H,
                SplitAxis::V => SplitDir::V,
            };
            let sum: f32 = ratios.iter().sum();
            let mut out_children = Vec::with_capacity(children.len());
            let mut out_ratios = Vec::with_capacity(children.len());
            for (c, r) in children.iter().zip(ratios) {
                let share = r / sum;
                match tree_node(c, ids) {
                    Node::Split {
                        dir: d,
                        ratios: rs,
                        children: cs,
                    } if d == dir => {
                        for (gc, gr) in cs.into_iter().zip(rs) {
                            out_children.push(gc);
                            out_ratios.push(share * gr);
                        }
                    }
                    other => {
                        out_children.push(other);
                        out_ratios.push(share);
                    }
                }
            }
            Node::Split {
                dir,
                ratios: out_ratios,
                children: out_children,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_layout::ItemKind;

    const AREA: egui::Vec2 = egui::vec2(1000.0, 800.0);

    /// A laid-out Project child manager with no windows.
    fn project() -> WindowManager {
        let mut wm = WindowManager::new();
        wm.cwd = Some(std::env::temp_dir());
        wm.tag = Some("p1".into());
        wm.last_area = AREA;
        wm
    }

    /// A cheap Content with a heap identity: an empty nested manager. Its
    /// `Box` address survives a move and changes on a rebuild.
    fn stub() -> Content {
        Content::Project(Box::new(WindowManager::new()))
    }

    fn image(name: &str) -> Content {
        Content::Image(crate::imageview::ImageView::error(name.into(), "test"))
    }

    /// Add a tiled single-tab window beside the focused one; returns the
    /// tab's layout id.
    fn tile(wm: &mut WindowManager, content: Content) -> LayoutItemId {
        let anchor = wm.focused;
        let (id, rect) = wm.next_slot(egui::vec2(400.0, 300.0));
        let tab = Tab::fixed("w", content);
        let item = tab.layout_id();
        wm.push_win(id, tab, rect);
        wm.tile_new(id, anchor);
        item
    }

    /// Add a floating single-tab window at `rect`; returns the layout id.
    fn float(wm: &mut WindowManager, content: Content, rect: egui::Rect) -> LayoutItemId {
        let (id, _) = wm.next_slot(rect.size());
        let tab = Tab::fixed("f", content);
        let item = tab.layout_id();
        wm.push_win(id, tab, rect);
        item
    }

    fn win_of(wm: &WindowManager, item: LayoutItemId) -> &Win {
        wm.windows
            .iter()
            .find(|w| w.tabs.iter().any(|t| t.layout_id() == item))
            .expect("item is in a window")
    }

    fn win_id_of(wm: &WindowManager, item: LayoutItemId) -> WinId {
        win_of(wm, item).id
    }

    /// Heap address of every stub, by layout id: the Content-identity probe.
    fn identities(wm: &WindowManager) -> Vec<(LayoutItemId, usize)> {
        let mut out: Vec<_> = wm
            .windows
            .iter()
            .flat_map(|w| w.tabs.iter())
            .filter_map(|t| match &t.content {
                Content::Project(b) => Some((t.layout_id(), &**b as *const WindowManager as usize)),
                _ => None,
            })
            .collect();
        out.sort();
        out
    }

    fn tab_count(wm: &WindowManager) -> usize {
        wm.windows.iter().map(|w| w.tabs.len()).sum()
    }

    fn snapshot(wm: &WindowManager) -> (Vec<ItemRef>, Arrangement) {
        wm.capture_arrangement(wm.last_area)
    }

    fn definition(id: &str, wm: &WindowManager) -> LayoutDefinition {
        let (items, arrangement) = snapshot(wm);
        hand_def(id, items, arrangement)
    }

    fn hand_def(id: &str, items: Vec<ItemRef>, arrangement: Arrangement) -> LayoutDefinition {
        LayoutDefinition {
            id: id.into(),
            name: id.into(),
            items,
            arrangement,
        }
    }

    fn sole_stack(tabs: Vec<LayoutItemId>) -> Arrangement {
        Arrangement {
            windows: vec![WindowSpec::tiled(tabs, 0)],
            tree: Some(TreeSpec::Leaf { window: 0 }),
        }
    }

    /// Tiled windows side by side (H split, equal ratios).
    fn row(n: usize) -> Option<TreeSpec> {
        Some(TreeSpec::Split {
            dir: SplitAxis::H,
            ratios: vec![1.0; n],
            children: (0..n).map(|window| TreeSpec::Leaf { window }).collect(),
        })
    }

    fn projects(ids: &[LayoutItemId]) -> Vec<ItemRef> {
        ids.iter()
            .map(|&id| ItemRef::new(id, ItemKind::Project))
            .collect()
    }

    /// Three tiled stubs a | b | c, captured as definition "a".
    fn three_tiles() -> (WindowManager, [LayoutItemId; 3], LayoutDefinition) {
        let mut wm = project();
        let a = tile(&mut wm, stub());
        let b = tile(&mut wm, stub());
        let c = tile(&mut wm, stub());
        let def = definition("a", &wm);
        (wm, [a, b, c], def)
    }

    #[test]
    fn apply_moves_every_content_object_and_closes_nothing() {
        let (mut wm, [a, b, c], _) = three_tiles();
        let before = identities(&wm);
        // One stack holding b and a (active); c floats.
        let def = hand_def(
            "stacked",
            projects(&[a, b, c]),
            Arrangement {
                windows: vec![
                    WindowSpec::tiled(vec![b, a], 1),
                    WindowSpec::floating(vec![c], 0, NormRect::new(0.5, 0.5, 0.3, 0.3)),
                ],
                tree: Some(TreeSpec::Leaf { window: 0 }),
            },
        );
        let ctx = egui::Context::default();
        wm.workspace_dirty = false;
        let applied = wm.apply_layout(&def, true, &ctx).expect("applies");

        assert!(applied.is_exact());
        assert_eq!(identities(&wm), before, "the same Content objects, moved");
        assert_eq!(wm.windows.len(), 2);
        let stack = win_of(&wm, a);
        let order: Vec<_> = stack.tabs.iter().map(|t| t.layout_id()).collect();
        assert_eq!(order, vec![b, a]);
        assert_eq!(stack.active, 1);
        assert_eq!(wm.tree.leaves(), vec![stack.id]);
        let floating = win_of(&wm, c);
        assert!(!wm.tree.contains(floating.id) && !floating.minimized);
        assert_eq!(floating.rect.min, egui::pos2(500.0, 400.0));
        assert_eq!(
            wm.layout,
            LayoutSelection {
                selected: Some("stacked".into()),
                modified: false
            }
        );
        assert!(
            wm.workspace_dirty,
            "the transition schedules a workspace save"
        );
    }

    #[test]
    fn switching_a_to_b_to_a_reuses_the_windows_b_set_aside() {
        let (mut wm, [a, b, c], def_a) = three_tiles();
        let before = identities(&wm);
        let (_, arranged_a) = snapshot(&wm);
        let def_b = hand_def("b", projects(&[a]), sole_stack(vec![a]));
        let ctx = egui::Context::default();

        let to_b = wm.apply_layout(&def_b, true, &ctx).unwrap();
        let extras: Vec<_> = to_b
            .reconciliation
            .additional
            .iter()
            .map(|i| i.id)
            .collect();
        assert_eq!(extras, vec![b, c]);
        assert_eq!(wm.windows.len(), 2, "a, plus one Additional windows stack");
        assert!(wm.layout.modified, "extras make the Apply dirty");

        let back = wm.apply_layout(&def_a, true, &ctx).unwrap();
        assert!(back.reconciliation.opened.is_empty());
        assert!(back.is_exact());
        assert!(!wm.layout.modified);
        assert_eq!(identities(&wm), before);
        assert_eq!(tab_count(&wm), 3, "nothing opened");
        assert!(pl::same_arrangement(&snapshot(&wm).1, &arranged_a));
    }

    #[test]
    fn revert_restores_geometry_selection_and_modified_without_rewinding_content() {
        let (mut wm, [a, ..], _) = three_tiles();
        let prior = LayoutSelection {
            selected: Some("old".into()),
            modified: true,
        };
        wm.layout = prior.clone();
        let (_, arranged) = snapshot(&wm);
        let before = identities(&wm);
        let ctx = egui::Context::default();
        let def_b = hand_def("b", projects(&[a]), sole_stack(vec![a]));
        wm.apply_layout(&def_b, true, &ctx).unwrap();
        assert_eq!(wm.layout_revert_status(), RevertStatus::Ready);

        wm.revert_layout().expect("reverts");
        assert_eq!(wm.layout, prior);
        assert_eq!(identities(&wm), before);
        assert!(pl::same_arrangement(&snapshot(&wm).1, &arranged));
        assert_eq!(
            wm.layout_revert_status(),
            RevertStatus::Unavailable,
            "one level"
        );
        assert_eq!(
            wm.revert_layout().unwrap_err(),
            LayoutApplyError::NoRevertPoint
        );
    }

    #[test]
    fn revert_keeps_apply_opened_items_open_in_additional_windows() {
        let mut wm = project();
        let a = tile(&mut wm, stub());
        let ctx = egui::Context::default();
        let def = hand_def(
            "with-chat",
            vec![
                ItemRef::new(a, ItemKind::Project),
                ItemRef::new(9_000, ItemKind::Chat),
            ],
            Arrangement {
                windows: vec![
                    WindowSpec::tiled(vec![a], 0),
                    WindowSpec::tiled(vec![9_000], 0),
                ],
                tree: row(2),
            },
        );
        let applied = wm.apply_layout(&def, true, &ctx).unwrap();
        let chat = applied.reconciliation.opened[0].current;
        assert!(applied.is_exact(), "a filled slot is a clean Apply");
        assert_eq!(
            wm.layout_revert_status(),
            RevertStatus::Ready,
            "opening is part of the Apply"
        );

        let reverted = wm.revert_layout().unwrap();
        assert_eq!(
            reverted.reconciliation.additional,
            vec![ItemRef::new(chat, ItemKind::Chat)]
        );
        assert_eq!(tab_count(&wm), 2, "the chat stays open");
        assert!(
            reverted.reconciliation.opened.is_empty(),
            "Revert opens nothing"
        );
    }

    #[test]
    fn fresh_views_open_for_missing_slots_and_target_kinds_go_unfilled() {
        let mut wm = project();
        let a = tile(&mut wm, stub());
        let ctx = egui::Context::default();
        let slots = [
            ItemRef::new(9_001, ItemKind::Chat),
            ItemRef::new(9_002, ItemKind::Board),
            ItemRef::new(9_003, ItemKind::Plan),
            ItemRef::new(9_004, ItemKind::GitHistory),
            ItemRef::new(9_005, ItemKind::GitChanges),
            ItemRef::new(9_006, ItemKind::Image),
            ItemRef::new(9_007, ItemKind::GitDiff),
        ];
        let windows: Vec<WindowSpec> = slots
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let at = 0.05 * i as f32;
                WindowSpec::floating(vec![s.id], 0, NormRect::new(at, at, 0.3, 0.3))
            })
            .collect();
        let def = hand_def(
            "views",
            slots.to_vec(),
            Arrangement {
                windows,
                tree: None,
            },
        );

        let applied = wm.apply_layout(&def, false, &ctx).unwrap();
        let opened: Vec<ItemKind> = applied
            .reconciliation
            .opened
            .iter()
            .map(|o| o.kind)
            .collect();
        assert_eq!(
            opened,
            vec![
                ItemKind::Chat,
                ItemKind::Board,
                ItemKind::Plan,
                ItemKind::GitHistory,
                ItemKind::GitChanges
            ]
        );
        assert_eq!(
            applied.reconciliation.unfilled,
            vec![slots[5], slots[6]],
            "a viewer that needs a target is never invented"
        );
        assert_eq!(applied.reconciliation.additional, projects(&[a]));
        assert_eq!(wm.layout_inventory().len(), 6);
        assert_eq!(
            wm.layout,
            LayoutSelection::default(),
            "a candidate stays Custom"
        );
    }

    #[test]
    fn multiple_viewers_of_one_kind_keep_their_targets() {
        let mut wm = project();
        let x = tile(&mut wm, image("x.png"));
        let y = tile(&mut wm, image("y.png"));
        let ctx = egui::Context::default();
        // Matching is by identity, so each viewer's target travels with it.
        let def = hand_def(
            "swap",
            vec![
                ItemRef::new(y, ItemKind::Image),
                ItemRef::new(x, ItemKind::Image),
            ],
            sole_stack(vec![y, x]),
        );
        wm.apply_layout(&def, true, &ctx).unwrap();
        let paths: Vec<std::path::PathBuf> = wm.windows[0]
            .tabs
            .iter()
            .map(|t| match &t.content {
                Content::Image(v) => v.path.clone(),
                _ => panic!("image"),
            })
            .collect();
        assert_eq!(
            paths,
            vec![std::path::PathBuf::from("y.png"), "x.png".into()]
        );
    }

    #[test]
    fn a_visible_slot_restores_a_minimized_substitute_and_minimized_extras_stay_put() {
        let mut wm = project();
        let a = tile(&mut wm, stub());
        let b = tile(&mut wm, stub());
        let c = tile(&mut wm, stub());
        wm.minimize(win_id_of(&wm, b));
        wm.minimize(win_id_of(&wm, c));
        let ctx = egui::Context::default();
        // Two visible Project slots that match nothing by id.
        let def = hand_def(
            "two",
            projects(&[9_100, 9_101]),
            Arrangement {
                windows: vec![
                    WindowSpec::tiled(vec![9_100], 0),
                    WindowSpec::tiled(vec![9_101], 0),
                ],
                tree: row(2),
            },
        );
        wm.apply_layout(&def, true, &ctx).unwrap();
        assert!(!win_of(&wm, a).minimized);
        assert!(
            !win_of(&wm, b).minimized,
            "a minimized window fills the free slot"
        );
        assert!(wm.tree.contains(win_id_of(&wm, b)));
        let kept = win_of(&wm, c);
        assert!(
            kept.minimized && kept.min_from_tree,
            "an unused minimized window stays put"
        );
        assert_eq!(tab_count(&wm), 3);
    }

    #[test]
    fn focus_follows_content_then_first_visible_and_is_empty_when_all_minimized() {
        let (mut wm, [a, b, c], _) = three_tiles();
        let ctx = egui::Context::default();
        wm.focus(win_id_of(&wm, b));
        let items = projects(&[c, b, a]);
        let reorder = hand_def(
            "reorder",
            items.clone(),
            Arrangement {
                windows: vec![
                    WindowSpec::tiled(vec![c], 0),
                    WindowSpec::tiled(vec![b], 0),
                    WindowSpec::tiled(vec![a], 0),
                ],
                tree: row(3),
            },
        );
        wm.apply_layout(&reorder, true, &ctx).unwrap();
        assert_eq!(wm.focused, Some(win_id_of(&wm, b)), "focused Content kept");

        // b becomes an inactive tab: no longer visible, so the first leaf wins.
        let stacked = hand_def(
            "stacked",
            items.clone(),
            Arrangement {
                windows: vec![
                    WindowSpec::tiled(vec![c], 0),
                    WindowSpec::tiled(vec![a, b], 0),
                ],
                tree: row(2),
            },
        );
        wm.apply_layout(&stacked, true, &ctx).unwrap();
        assert_eq!(wm.focused, Some(win_id_of(&wm, c)));

        let rect = NormRect::new(0.1, 0.1, 0.4, 0.4);
        let hidden = hand_def(
            "hidden",
            items,
            Arrangement {
                windows: vec![WindowSpec::minimized(vec![a, b, c], 0, false, rect)],
                tree: None,
            },
        );
        wm.apply_layout(&hidden, true, &ctx).unwrap();
        assert!(wm.windows.iter().all(|w| w.minimized));
        assert_eq!(wm.focused, None, "no child keyboard target");
    }

    #[test]
    fn apply_exits_child_zoom_and_confines_floating_geometry() {
        let mut wm = project();
        let a = tile(&mut wm, stub());
        let f = float(
            &mut wm,
            stub(),
            egui::Rect::from_min_size(egui::pos2(100.0, 100.0), egui::vec2(300.0, 200.0)),
        );
        wm.toggle_zoom(win_id_of(&wm, a));
        let ctx = egui::Context::default();
        let def = hand_def(
            "off-screen",
            projects(&[a, f]),
            Arrangement {
                windows: vec![
                    WindowSpec::tiled(vec![a], 0),
                    WindowSpec::floating(vec![f], 0, NormRect::new(0.9, 0.9, 0.01, 0.01)),
                ],
                tree: Some(TreeSpec::Leaf { window: 0 }),
            },
        );
        wm.apply_layout(&def, true, &ctx).unwrap();
        assert_eq!(wm.zoomed, None);
        let r = win_of(&wm, f).rect;
        assert!(
            r.width() >= MIN_W && r.height() >= MIN_H,
            "size floor: {r:?}"
        );
        assert!(
            r.max.x <= AREA.x && r.max.y <= AREA.y,
            "inside the area: {r:?}"
        );
    }

    #[test]
    fn invalid_or_blocked_transitions_leave_the_arrangement_intact() {
        let (mut wm, [a, b, _], _) = three_tiles();
        let ctx = egui::Context::default();
        let before = snapshot(&wm);
        let dup = hand_def(
            "dup",
            projects(&[a, b]),
            Arrangement {
                windows: vec![WindowSpec::tiled(vec![a, a], 0)],
                tree: Some(TreeSpec::Leaf { window: 0 }),
            },
        );
        assert!(matches!(
            wm.apply_layout(&dup, true, &ctx),
            Err(LayoutApplyError::Invalid(_))
        ));
        let nan_rect = NormRect::new(f32::NAN, 0.0, 0.5, 0.5);
        let nan = hand_def(
            "nan",
            projects(&[a]),
            Arrangement {
                windows: vec![WindowSpec::floating(vec![a], 0, nan_rect)],
                tree: None,
            },
        );
        assert!(wm.apply_layout(&nan, true, &ctx).is_err());

        let ok = hand_def("ok", projects(&[a]), sole_stack(vec![a]));
        wm.pending_close = Some(PendingClose {
            target: CloseTarget::ActiveTab(win_id_of(&wm, a)),
            view: crate::confirm::ConfirmClose::new("t", "l", "c", Vec::new()),
        });
        assert_eq!(
            wm.apply_layout(&ok, true, &ctx).unwrap_err(),
            LayoutApplyError::Busy
        );
        wm.pending_close = None;

        assert_eq!(snapshot(&wm), before);
        assert_eq!(wm.layout, LayoutSelection::default());
        assert_eq!(wm.layout_revert_status(), RevertStatus::Unavailable);

        let mut desktop = WindowManager::new().as_desktop();
        desktop.last_area = AREA;
        assert_eq!(
            desktop.apply_layout(&ok, true, &ctx).unwrap_err(),
            LayoutApplyError::NotAProject
        );
    }

    #[test]
    fn modified_is_set_by_structural_edits_only() {
        let (mut wm, [a, b, _], def) = three_tiles();
        let ctx = egui::Context::default();
        wm.apply_layout(&def, true, &ctx).unwrap();
        assert!(!wm.layout.modified, "an exact Apply is clean");
        wm.watch_layout_edits();

        // Focus, title, zoom: not structural.
        wm.focus(win_id_of(&wm, b));
        wm.windows[0].tabs[0].title = "agent renamed me".into();
        wm.toggle_zoom(win_id_of(&wm, a));
        wm.mark_workspace_dirty();
        wm.watch_layout_edits();
        wm.unzoom();
        wm.mark_workspace_dirty();
        wm.watch_layout_edits();
        assert!(!wm.layout.modified);
        assert_eq!(wm.layout_revert_status(), RevertStatus::Ready);

        // A Project resize only re-measures.
        wm.last_area = egui::vec2(600.0, 500.0);
        wm.mark_workspace_dirty();
        wm.watch_layout_edits();
        wm.mark_workspace_dirty();
        wm.watch_layout_edits();
        assert!(!wm.layout.modified);
        assert_eq!(
            wm.layout_revert_status(),
            RevertStatus::Ready,
            "survives resizing"
        );

        // Minimize is structural: Modified, and the revert point is retired.
        wm.minimize(win_id_of(&wm, b));
        wm.mark_workspace_dirty();
        wm.watch_layout_edits();
        assert!(wm.layout.modified);
        assert_eq!(wm.layout_revert_status(), RevertStatus::Invalidated);
        let before = snapshot(&wm);
        assert_eq!(
            wm.revert_layout().unwrap_err(),
            LayoutApplyError::RevertInvalidated
        );
        assert_eq!(snapshot(&wm), before);

        // Save clears it.
        wm.mark_layout_saved("a".into());
        assert!(!wm.layout.modified);
    }

    #[test]
    fn floating_resize_clamps_do_not_count_as_edits() {
        let mut wm = project();
        tile(&mut wm, stub());
        float(
            &mut wm,
            stub(),
            egui::Rect::from_min_size(egui::pos2(600.0, 500.0), egui::vec2(300.0, 250.0)),
        );
        let def = definition("sized", &wm);
        let ctx = egui::Context::default();
        wm.apply_layout(&def, true, &ctx).unwrap();
        // What `show` does on a shrink: clamp the float into the new area,
        // then the end-of-frame watch runs.
        wm.last_area = egui::vec2(500.0, 400.0);
        for w in wm.windows.iter_mut().filter(|w| !wm.tree.contains(w.id)) {
            clamp(&mut w.rect, egui::vec2(500.0, 400.0));
        }
        wm.watch_layout_edits();
        wm.focus(wm.windows[1].id);
        wm.mark_workspace_dirty();
        wm.watch_layout_edits();
        assert!(!wm.layout.modified);
    }

    #[test]
    fn divider_moves_and_closing_content_are_structural() {
        let (mut wm, [a, b, _], def) = three_tiles();
        let ctx = egui::Context::default();
        wm.apply_layout(&def, true, &ctx).unwrap();
        let local = egui::Rect::from_min_size(egui::Pos2::ZERO, AREA);
        let id = win_id_of(&wm, a);
        wm.tree.resize_edge(id, Dir::Right, 60.0, local, SNAP_GAP);
        wm.mark_workspace_dirty();
        wm.watch_layout_edits();
        assert!(wm.layout.modified);

        wm.mark_layout_saved("a".into());
        wm.close(win_id_of(&wm, b));
        wm.watch_layout_edits();
        assert!(wm.layout.modified);
    }

    #[test]
    fn apply_at_a_different_size_from_the_save_is_clean() {
        let mut wm = project();
        tile(&mut wm, stub());
        float(
            &mut wm,
            stub(),
            egui::Rect::from_min_size(egui::pos2(600.0, 500.0), egui::vec2(300.0, 250.0)),
        );
        let def = definition("sized", &wm);
        wm.last_area = egui::vec2(640.0, 400.0);
        let ctx = egui::Context::default();
        let applied = wm.apply_layout(&def, true, &ctx).unwrap();
        assert!(applied.is_exact());
        assert!(!wm.layout.modified);
        wm.mark_workspace_dirty();
        wm.watch_layout_edits();
        assert!(!wm.layout.modified);
    }

    #[test]
    fn requests_run_through_the_deferred_boundary() {
        let (mut wm, [a, ..], _) = three_tiles();
        let ctx = egui::Context::default();
        let def = hand_def("b", projects(&[a]), sole_stack(vec![a]));
        wm.request_layout(LayoutRequest::Apply { def, select: true });
        assert_eq!(wm.windows.len(), 3, "nothing moves until the next frame");
        wm.drain_layout_request(&ctx);
        assert!(matches!(wm.take_layout_outcome(), Some(Ok(_))));
        assert_eq!(wm.windows.len(), 2);
        wm.request_layout(LayoutRequest::Revert);
        wm.drain_layout_request(&ctx);
        assert!(matches!(wm.take_layout_outcome(), Some(Ok(_))));
        assert_eq!(wm.windows.len(), 3);
        assert!(wm.take_layout_outcome().is_none());
    }

    #[test]
    fn apply_in_one_project_leaves_the_desktop_and_other_projects_alone() {
        let (p1, [a, ..], _) = three_tiles();
        let (mut p2, ..) = three_tiles();
        p2.tag = Some("p2".into());
        let mut desk = WindowManager::new().as_desktop();
        desk.last_area = egui::vec2(1600.0, 900.0);
        let r1 = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 900.0));
        let r2 = egui::Rect::from_min_size(egui::pos2(800.0, 0.0), egui::vec2(800.0, 900.0));
        desk.push_win(1, Tab::fixed("p1", Content::Project(Box::new(p1))), r1);
        desk.push_win(2, Tab::fixed("p2", Content::Project(Box::new(p2))), r2);
        desk.next = 3;
        let other_before = desk.project_child(2).map(snapshot).unwrap();
        let ctx = egui::Context::default();
        let def = hand_def("b", projects(&[a]), sole_stack(vec![a]));
        desk.project_child_mut(1)
            .unwrap()
            .apply_layout(&def, true, &ctx)
            .unwrap();
        assert_eq!(desk.project_child(2).map(snapshot).unwrap(), other_before);
        assert_eq!(desk.windows[0].rect, r1);
        assert_eq!(desk.windows[1].rect, r2);
        assert_eq!(
            desk.project_child(2).unwrap().layout,
            LayoutSelection::default()
        );
    }

    // ---- real Sessions: process preservation through Inspection ----

    fn cmd_settings(ctx: &egui::Context) {
        let mut s = crate::config::Settings::default();
        s.default_shell = crate::config::DefaultShell::Cmd;
        crate::config::seed_live(ctx, &s);
    }

    fn snapshot_text(desk: &mut WindowManager, ctx: &egui::Context, term: WinId) -> String {
        let (tx, rx) = std::sync::mpsc::channel();
        let req = crate::control::SnapshotRequest {
            cmd: "snapshot".into(),
            project: Some("p1".into()),
            terminal: Some(format!("t{term}")),
            attrs: false,
            cursor: false,
            tail: None,
        };
        let msg = crate::control::CtrlMsg::Snapshot(req, tx, std::time::Instant::now());
        desk.handle_ctrl(msg, ctx);
        let reply = rx.try_recv().expect("snapshot reply");
        assert!(reply.ok, "{:?}", reply.error);
        reply.history.unwrap_or_default().join("\n")
    }

    fn send_line(desk: &mut WindowManager, ctx: &egui::Context, term: WinId, text: &str) {
        let (tx, rx) = std::sync::mpsc::channel();
        let req = crate::control::SendRequest {
            cmd: "send".into(),
            project: Some("p1".into()),
            terminal: Some(format!("t{term}")),
            text: Some(text.into()),
            keys: vec!["Enter".into()],
            settle_ms: Some(0),
        };
        let msg = crate::control::CtrlMsg::Send(req, tx, std::time::Instant::now());
        desk.handle_ctrl(msg, ctx);
        let reply = rx.try_recv().expect("send reply");
        assert!(reply.ok, "{:?}", reply.error);
    }

    fn wait_for(desk: &mut WindowManager, ctx: &egui::Context, term: WinId, needle: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let text = snapshot_text(desk, ctx, term);
            if text.contains(needle) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "{needle:?} never appeared:\n{text}"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    fn child(desk: &mut WindowManager) -> &mut WindowManager {
        desk.project_child_mut(1).expect("project p1")
    }

    /// (layout id, Member id, root pid) of every terminal in the Project.
    fn sessions(desk: &mut WindowManager) -> Vec<(LayoutItemId, u64, Option<u32>)> {
        let mut out: Vec<_> = child(desk)
            .windows
            .iter()
            .flat_map(|w| w.tabs.iter())
            .filter_map(|t| match &t.content {
                Content::Terminal(s) => Some((t.layout_id(), s.term_id(), s.root_pid())),
                _ => None,
            })
            .collect();
        out.sort();
        out
    }

    #[test]
    fn apply_and_revert_keep_live_sessions_usable_through_inspection() {
        let ctx = egui::Context::default();
        cmd_settings(&ctx);
        let mut inner = project();
        let (t1, tab1) = inner.spawn_shell_tab(Shell::Cmd, &ctx).unwrap();
        let (t2, tab2) = inner.spawn_shell_tab(Shell::Cmd, &ctx).unwrap();
        let (i1, i2) = (tab1.layout_id(), tab2.layout_id());
        let full = egui::Rect::from_min_size(egui::Pos2::ZERO, AREA);
        inner.push_win(t1, tab1, full);
        inner.tile_new(t1, None);
        inner.push_win(t2, tab2, full);
        inner.tile_new(t2, Some(t1));
        let split = definition("split", &inner);
        let mut desk = WindowManager::new().as_desktop();
        desk.push_win(
            1,
            Tab::fixed("proj", Content::Project(Box::new(inner))),
            full,
        );

        wait_for(&mut desk, &ctx, t1, ">");
        // cmd expands %OS% only when it runs the line, so the needle matches
        // shell output, never the echoed command line.
        send_line(&mut desk, &ctx, t1, "echo BEFORE_%OS%");
        wait_for(&mut desk, &ctx, t1, "BEFORE_Windows_NT");
        let before = sessions(&mut desk);
        assert_eq!(before.len(), 2);
        assert!(before.iter().all(|s| s.2.is_some()));

        // B: both terminals as tabs of one stack (t1 inactive), then back.
        let items = vec![
            ItemRef::new(i2, ItemKind::Terminal),
            ItemRef::new(i1, ItemKind::Terminal),
        ];
        let stacked = hand_def("stacked", items, sole_stack(vec![i2, i1]));
        let to_b = child(&mut desk).apply_layout(&stacked, true, &ctx).unwrap();
        assert!(to_b.is_exact() && to_b.reconciliation.opened.is_empty());
        assert_eq!(
            sessions(&mut desk),
            before,
            "same Sessions, Member ids, processes"
        );
        wait_for(&mut desk, &ctx, t1, "BEFORE_Windows_NT");

        child(&mut desk).apply_layout(&split, true, &ctx).unwrap();
        child(&mut desk).apply_layout(&stacked, true, &ctx).unwrap();
        child(&mut desk).revert_layout().unwrap();
        assert_eq!(child(&mut desk).windows.len(), 2, "reverted to the split");
        assert_eq!(sessions(&mut desk), before);

        // The grid kept its output, and the shell still takes input.
        send_line(&mut desk, &ctx, t1, "echo AFTER_%OS%");
        wait_for(&mut desk, &ctx, t1, "AFTER_Windows_NT");
        let text = snapshot_text(&mut desk, &ctx, t1);
        assert!(text.contains("BEFORE_Windows_NT"), "grid kept:\n{text}");
    }

    #[test]
    fn a_failed_open_omits_its_slot_and_is_reported_without_aborting() {
        let ctx = egui::Context::default();
        cmd_settings(&ctx);
        let mut wm = project();
        let a = tile(&mut wm, stub());
        let b = tile(&mut wm, stub());
        let slot = ItemRef::new(9_300, ItemKind::Terminal);
        let def = hand_def(
            "three",
            vec![
                ItemRef::new(a, ItemKind::Project),
                slot,
                ItemRef::new(b, ItemKind::Project),
            ],
            Arrangement {
                windows: vec![
                    WindowSpec::tiled(vec![a], 0),
                    WindowSpec::tiled(vec![slot.id], 0),
                    WindowSpec::tiled(vec![b], 0),
                ],
                tree: row(3),
            },
        );
        wm.layout_rt.fail_opens = true;
        let applied = wm.apply_layout(&def, true, &ctx).expect("Apply goes on");
        assert_eq!(applied.failed_opens.len(), 1);
        assert_eq!(applied.failed_opens[0].saved, slot);
        assert!(applied.reconciliation.unfilled.contains(&slot));
        assert!(applied.reconciliation.opened.is_empty());
        assert_eq!(wm.windows.len(), 2, "the slot left the arrangement");
        assert_eq!(wm.tree.leaves().len(), 2);
        assert!(wm.layout.modified, "an unfilled slot is not a clean Apply");
    }

    #[test]
    fn a_fresh_terminal_slot_opens_the_default_shell_with_no_command() {
        let ctx = egui::Context::default();
        cmd_settings(&ctx);
        let mut wm = project();
        let a = tile(&mut wm, stub());
        let chat_before = wm.chat.borrow().last_seq();
        let def = hand_def(
            "two",
            vec![
                ItemRef::new(a, ItemKind::Project),
                ItemRef::new(9_200, ItemKind::Terminal),
            ],
            Arrangement {
                windows: vec![
                    WindowSpec::tiled(vec![a], 0),
                    WindowSpec::tiled(vec![9_200], 0),
                ],
                tree: row(2),
            },
        );
        let applied = wm.apply_layout(&def, true, &ctx).unwrap();
        assert!(
            applied.failed_opens.is_empty(),
            "{:?}",
            applied.failed_opens
        );
        assert!(applied.is_exact());
        let id = applied.reconciliation.opened[0].current;
        let tab = wm
            .windows
            .iter()
            .flat_map(|w| w.tabs.iter())
            .find(|t| t.layout_id() == id)
            .expect("opened tab");
        let Content::Terminal(s) = &tab.content else {
            panic!("a terminal")
        };
        assert_eq!(s.shell, Shell::Cmd);
        assert!(tab.auto_title, "a plain shell, named like any new terminal");
        assert!(tab.title.starts_with(Shell::Cmd.label()));
        assert_eq!(
            wm.chat.borrow().last_seq(),
            chat_before,
            "no agent joined chat"
        );
        assert!(!wm.chat.borrow().is_member(&term_tag(s.term_id())));
    }
}
