//! Cold workspace snapshot: types + serde + `%APPDATA%\foreman\workspace.json`
//! load/save, pure layout-tree ↔ `NodeSnap` conversion, and capture of a live
//! `WindowManager` into `ManagerSnap` / `WorkspaceSnapshot`. Apply and dirty-flag
//! wiring live in later tasks.
//!
//! Mirrors `recents.rs` / `config.rs`: defaults in code, corruption-tolerant
//! load, atomic save via `config::save_json`. Future file versions are rejected
//! (not partially applied) so a newer foreman never leaves a half-restored tree.

use crate::layout::{LayoutTree, MIN_RATIO, Node, SplitDir};
use crate::wm::WinId;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Stable id in a snapshot. Matches live `WinId` as `u64` at capture time.
pub type SnapId = u64;

pub const WORKSPACE_FILE: &str = "workspace.json";
pub const WORKSPACE_VERSION: u32 = 1;

/// Root document written to `workspace.json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceSnapshot {
    pub version: u32,
    pub desktop: ManagerSnap,
}

impl Default for WorkspaceSnapshot {
    fn default() -> Self {
        Self {
            // Prefer v1 on empty/default so bare saves and `{}` loads agree.
            version: 1,
            desktop: ManagerSnap::default(),
        }
    }
}

/// One `WindowManager` level (desktop or nested project child).
///
/// `windows` z-order: **low index = back, high index = front**.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ManagerSnap {
    pub cwd: Option<PathBuf>,
    pub focused: Option<SnapId>,
    pub last_focused: Option<SnapId>,
    pub zoomed: Option<SnapId>,
    pub windows: Vec<WinSnap>,
    pub tree: Option<NodeSnap>,
}

impl ManagerSnap {
    /// True when any tab at this level is a project window.
    pub fn has_project(&self) -> bool {
        self.windows.iter().any(|w| {
            w.tabs
                .iter()
                .any(|t| matches!(t.content, ContentSnap::Project { .. }))
        })
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WinSnap {
    pub id: SnapId,
    pub active: usize,
    pub tabs: Vec<TabSnap>,
    pub minimized: bool,
    pub min_from_tree: bool,
    pub rect: RectSnap,
    pub prev: Option<RectSnap>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct TabSnap {
    pub title: String,
    /// True when Foreman owns the title. Managed task names are intentionally
    /// not persisted; restore starts from a fresh shell label and may name the
    /// next agent session from its own first prompt.
    pub managed_title: bool,
    pub content: ContentSnap,
}

impl Default for TabSnap {
    fn default() -> Self {
        Self {
            title: String::new(),
            managed_title: false,
            content: ContentSnap::Chat,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ContentSnap {
    Terminal {
        shell: String,
    },
    Chat,
    Project {
        child: ManagerSnap,
    },
    /// `foreman view` window. Path only, v1 — zoom/pan are not persisted; a
    /// restored viewer always opens fit-to-window. A path that no longer
    /// resolves (or no longer decodes) restores into the placeholder state,
    /// same as a live `foreman view` of a bad path.
    Image {
        path: PathBuf,
    },
}

/// Layout tree node. `dir` is `"H"` or `"V"` for splits.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum NodeSnap {
    Leaf {
        id: SnapId,
    },
    Split {
        dir: String,
        ratios: Vec<f32>,
        children: Vec<NodeSnap>,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RectSnap {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl WorkspaceSnapshot {
    /// Load from `workspace.json`, falling back to defaults on any problem.
    /// Future versions are ignored entirely (not partially applied).
    pub fn load() -> Self {
        let s: Self = crate::config::load_json(WORKSPACE_FILE);
        finalize_snapshot(s)
    }

    /// Persist atomically to `workspace.json`.
    pub fn save(&self) -> Result<(), String> {
        crate::config::save_json(WORKSPACE_FILE, self)
    }

    /// True when the desktop has no project windows to restore.
    pub fn is_empty(&self) -> bool {
        !self.desktop.has_project()
    }
}

/// Parse snapshot JSON with version policy. Used by tests and keeps rejection
/// unit-testable without touching `%APPDATA%`.
pub fn parse_workspace_json(text: &str) -> WorkspaceSnapshot {
    let s: WorkspaceSnapshot = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return WorkspaceSnapshot::default(),
    };
    finalize_snapshot(s)
}

fn finalize_snapshot(mut s: WorkspaceSnapshot) -> WorkspaceSnapshot {
    if s.version > WORKSPACE_VERSION {
        eprintln!(
            "foreman: workspace.json version {} is newer than supported {} — ignoring",
            s.version, WORKSPACE_VERSION
        );
        return WorkspaceSnapshot::default();
    }
    // Missing version on hand-written files: serde default is 0 when the field
    // is present as null or when older custom Default was 0 — treat as v1.
    // Note: `{}` uses `Default` so version is already 1; only explicit 0 needs lift.
    if s.version == 0 {
        s.version = 1;
    }
    s
}

/// Persist shell as a stable string (not the Rust enum name).
pub fn shell_to_str(shell: crate::terminal::Shell) -> &'static str {
    match shell {
        crate::terminal::Shell::PowerShell => "powershell",
        crate::terminal::Shell::Cmd => "cmd",
        crate::terminal::Shell::Bash => "bash",
    }
}

/// Snapshot an egui rect as absolute min + size (local to the manager area).
pub fn rect_to_snap(r: eframe::egui::Rect) -> RectSnap {
    RectSnap {
        x: r.min.x,
        y: r.min.y,
        w: r.width(),
        h: r.height(),
    }
}

/// Capture a live manager into a pure snapshot. Delegates to
/// [`crate::wm::WindowManager::capture_manager`] so private fields stay private.
///
/// Live `WinId` values are stored as `SnapId`s (identity). Restore still
/// allocates fresh runtime ids and remaps.
pub fn capture_manager(wm: &crate::wm::WindowManager) -> ManagerSnap {
    wm.capture_manager()
}

/// Parse a shell string; unknown values degrade to PowerShell.
pub fn shell_from_str(s: &str) -> crate::terminal::Shell {
    match s {
        "powershell" => crate::terminal::Shell::PowerShell,
        "cmd" => crate::terminal::Shell::Cmd,
        "bash" => crate::terminal::Shell::Bash,
        _ => crate::terminal::Shell::PowerShell,
    }
}

// ── Layout tree ↔ NodeSnap conversion (pure) ───────────────────────────────

/// Convert a live layout node to a snapshot node via `map` for leaf ids.
pub fn node_to_snap(n: &Node, map: &dyn Fn(WinId) -> SnapId) -> NodeSnap {
    match n {
        Node::Leaf(id) => NodeSnap::Leaf { id: map(*id) },
        Node::Split {
            dir,
            ratios,
            children,
        } => NodeSnap::Split {
            dir: match dir {
                SplitDir::H => "H".into(),
                SplitDir::V => "V".into(),
            },
            ratios: ratios.clone(),
            children: children.iter().map(|c| node_to_snap(c, map)).collect(),
        },
    }
}

/// Convert a snapshot node to a live layout node. Returns `None` if a leaf id
/// fails to map (caller drops that branch). Splits collapse after filtering:
/// 0 children → `None`, 1 child → that child, 2+ → re-normalized split.
pub fn node_from_snap(n: &NodeSnap, map: &dyn Fn(SnapId) -> Option<WinId>) -> Option<Node> {
    match n {
        NodeSnap::Leaf { id } => map(*id).map(Node::Leaf),
        NodeSnap::Split {
            dir,
            ratios,
            children,
        } => {
            let mut kept_children = Vec::new();
            let mut kept_ratios = Vec::new();
            for (i, child) in children.iter().enumerate() {
                if let Some(node) = node_from_snap(child, map) {
                    kept_children.push(node);
                    kept_ratios.push(ratios.get(i).copied().unwrap_or(0.0));
                }
            }
            match kept_children.len() {
                0 => None,
                1 => Some(kept_children.into_iter().next().unwrap()),
                n => {
                    let split_dir = match dir.as_str() {
                        "V" | "v" => SplitDir::V,
                        _ => SplitDir::H,
                    };
                    Some(Node::Split {
                        dir: split_dir,
                        ratios: fix_ratios(&kept_ratios, n),
                        children: kept_children,
                    })
                }
            }
        }
    }
}

/// Snapshot the tree root, if any.
pub fn tree_to_snap(tree: &LayoutTree, map: &dyn Fn(WinId) -> SnapId) -> Option<NodeSnap> {
    tree.root.as_ref().map(|n| node_to_snap(n, map))
}

/// Rebuild a `LayoutTree` from an optional snapshot root.
pub fn tree_from_snap(
    snap: Option<&NodeSnap>,
    map: &dyn Fn(SnapId) -> Option<WinId>,
) -> LayoutTree {
    LayoutTree {
        root: snap.and_then(|n| node_from_snap(n, map)),
    }
}

/// Fix ratios to match `n` children and sum to ~1.0.
///
/// If `ratios.len() != n` or the sum is ≈ 0, redistribute equal weights.
/// When the set is a full match, clamp each ratio ≥ `MIN_RATIO` then
/// renormalize so the vector sums to 1.0. (No new tree math beyond that.)
fn fix_ratios(ratios: &[f32], n: usize) -> Vec<f32> {
    if n == 0 {
        return Vec::new();
    }
    let sum: f32 = ratios.iter().copied().sum();
    if ratios.len() != n || sum.abs() < 1e-6 {
        return vec![1.0 / n as f32; n];
    }
    // Full set: clamp ≥ MIN_RATIO, then renormalize to sum 1.0.
    let mut out: Vec<f32> = ratios.iter().map(|r| r.max(MIN_RATIO)).collect();
    let s: f32 = out.iter().copied().sum();
    if s.abs() < 1e-6 {
        return vec![1.0 / n as f32; n];
    }
    for r in &mut out {
        *r /= s;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_object_loads_as_default() {
        // Default::version is 1 so `{}` and a hand-built default agree after load().
        let s: WorkspaceSnapshot = serde_json::from_str("{}").unwrap();
        assert_eq!(s.version, 1);
        assert!(s.desktop.windows.is_empty());
    }

    #[test]
    fn missing_fields_default() {
        let s: WorkspaceSnapshot = serde_json::from_str(r#"{"version":1,"desktop":{}}"#).unwrap();
        assert_eq!(s.version, 1);
        assert!(s.desktop.focused.is_none());
        assert!(s.desktop.windows.is_empty());
        assert!(s.desktop.tree.is_none());
    }

    #[test]
    fn known_layout_round_trips() {
        let snap = WorkspaceSnapshot {
            version: 1,
            desktop: ManagerSnap {
                cwd: None,
                focused: Some(1),
                last_focused: None,
                zoomed: None,
                windows: vec![WinSnap {
                    id: 1,
                    active: 0,
                    tabs: vec![TabSnap {
                        title: "foreman".into(),
                        managed_title: false,
                        content: ContentSnap::Project {
                            child: ManagerSnap {
                                cwd: Some(std::path::PathBuf::from(r"C:\code\foreman")),
                                focused: Some(2),
                                last_focused: None,
                                zoomed: None,
                                windows: vec![WinSnap {
                                    id: 2,
                                    active: 0,
                                    tabs: vec![TabSnap {
                                        title: "powershell  ·  #1".into(),
                                        managed_title: true,
                                        content: ContentSnap::Terminal {
                                            shell: "powershell".into(),
                                        },
                                    }],
                                    minimized: false,
                                    min_from_tree: false,
                                    rect: RectSnap {
                                        x: 0.0,
                                        y: 0.0,
                                        w: 400.0,
                                        h: 300.0,
                                    },
                                    prev: None,
                                }],
                                tree: Some(NodeSnap::Leaf { id: 2 }),
                            },
                        },
                    }],
                    minimized: false,
                    min_from_tree: false,
                    rect: RectSnap {
                        x: 10.0,
                        y: 10.0,
                        w: 720.0,
                        h: 480.0,
                    },
                    prev: None,
                }],
                tree: Some(NodeSnap::Leaf { id: 1 }),
            },
        };
        let json = serde_json::to_string_pretty(&snap).unwrap();
        let back: WorkspaceSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.version, 1);
        assert_eq!(back.desktop.focused, Some(1));
        let ContentSnap::Project { child } = &back.desktop.windows[0].tabs[0].content else {
            panic!("expected project");
        };
        assert_eq!(
            child.cwd.as_deref(),
            Some(std::path::Path::new(r"C:\code\foreman"))
        );
        assert!(matches!(
            &child.windows[0].tabs[0].content,
            ContentSnap::Terminal { shell } if shell == "powershell"
        ));
    }

    #[test]
    fn image_content_round_trips() {
        let snap = TabSnap {
            title: "armed.png".into(),
            managed_title: false,
            content: ContentSnap::Image {
                path: PathBuf::from(r"C:\shots\armed.png"),
            },
        };
        let json = serde_json::to_string(&snap).unwrap();
        assert!(json.contains(r#""kind":"Image""#), "{json}");
        let back: TabSnap = serde_json::from_str(&json).unwrap();
        match back.content {
            ContentSnap::Image { path } => assert_eq!(path, PathBuf::from(r"C:\shots\armed.png")),
            _ => panic!("expected Image"),
        }
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let s: WorkspaceSnapshot =
            serde_json::from_str(r#"{"version":1,"desktop":{},"future_top":true}"#).unwrap();
        assert_eq!(s.version, 1);
    }

    #[test]
    fn shell_round_trip_and_unknown_defaults() {
        use crate::terminal::Shell;
        assert_eq!(shell_to_str(Shell::Cmd), "cmd");
        assert_eq!(shell_from_str("bash"), Shell::Bash);
        assert_eq!(shell_from_str("nope"), Shell::PowerShell);
    }

    #[test]
    fn load_rejects_future_version() {
        // Implement via a free function used by load(), testable without disk:
        // parse_workspace_json(text) -> WorkspaceSnapshot
        let s = parse_workspace_json(r#"{"version":99,"desktop":{"windows":[{"id":1}]}}"#);
        assert!(
            s.desktop.windows.is_empty(),
            "future version must not partially load"
        );
    }

    #[test]
    fn is_empty_when_no_project_content() {
        let empty = WorkspaceSnapshot::default();
        assert!(empty.is_empty());

        let with_term = WorkspaceSnapshot {
            version: 1,
            desktop: ManagerSnap {
                windows: vec![WinSnap {
                    id: 1,
                    tabs: vec![TabSnap {
                        title: "t".into(),
                        managed_title: false,
                        content: ContentSnap::Terminal {
                            shell: "powershell".into(),
                        },
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            },
        };
        assert!(with_term.is_empty());

        let with_project = WorkspaceSnapshot {
            version: 1,
            desktop: ManagerSnap {
                windows: vec![WinSnap {
                    id: 1,
                    tabs: vec![TabSnap {
                        title: "p".into(),
                        managed_title: false,
                        content: ContentSnap::Project {
                            child: ManagerSnap::default(),
                        },
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            },
        };
        assert!(!with_project.is_empty());
    }

    #[test]
    fn version_zero_is_treated_as_one() {
        let s = parse_workspace_json(r#"{"version":0,"desktop":{}}"#);
        assert_eq!(s.version, 1);
    }

    #[test]
    fn tree_round_trip_preserves_h_split_ratios() {
        use crate::layout::{LayoutTree, Node, SplitDir};
        let tree = LayoutTree {
            root: Some(Node::Split {
                dir: SplitDir::H,
                ratios: vec![0.3, 0.7],
                children: vec![Node::Leaf(10), Node::Leaf(20)],
            }),
        };
        let to_snap = |id: WinId| id; // identity
        let snap = tree_to_snap(&tree, &to_snap).unwrap();
        let from_snap = |id: SnapId| Some(id);
        let back = tree_from_snap(Some(&snap), &from_snap);
        match back.root.unwrap() {
            Node::Split {
                dir,
                ratios,
                children,
            } => {
                assert_eq!(dir, SplitDir::H);
                assert!((ratios[0] - 0.3).abs() < 1e-5);
                assert!((ratios[1] - 0.7).abs() < 1e-5);
                assert!(matches!(children[0], Node::Leaf(10)));
                assert!(matches!(children[1], Node::Leaf(20)));
            }
            _ => panic!("expected split"),
        }
    }

    #[test]
    fn tree_from_snap_drops_unmapped_leaves() {
        let snap = NodeSnap::Split {
            dir: "V".into(),
            ratios: vec![0.5, 0.5],
            children: vec![
                NodeSnap::Leaf { id: 1 },
                NodeSnap::Leaf { id: 999 }, // unmapped
            ],
        };
        let map = |id: SnapId| if id == 1 { Some(5) } else { None };
        let tree = tree_from_snap(Some(&snap), &map);
        // Sole remaining leaf becomes root leaf 5 (collapse split with one child)
        assert!(matches!(tree.root, Some(Node::Leaf(5))));
    }

    #[test]
    fn fix_ratios_equal_split_on_mismatch() {
        // length mismatch → equal weights
        let r = fix_ratios(&[0.3], 2);
        assert_eq!(r.len(), 2);
        assert!((r[0] - 0.5).abs() < 1e-5);
        assert!((r[1] - 0.5).abs() < 1e-5);

        // sum ≈ 0 → equal weights
        let r = fix_ratios(&[0.0, 0.0], 2);
        assert!((r[0] - 0.5).abs() < 1e-5);
    }

    #[test]
    fn nested_split_round_trip() {
        let tree = LayoutTree {
            root: Some(Node::Split {
                dir: SplitDir::V,
                ratios: vec![0.4, 0.6],
                children: vec![
                    Node::Leaf(1),
                    Node::Split {
                        dir: SplitDir::H,
                        ratios: vec![0.25, 0.75],
                        children: vec![Node::Leaf(2), Node::Leaf(3)],
                    },
                ],
            }),
        };
        let to_snap = |id: WinId| id;
        let snap = tree_to_snap(&tree, &to_snap).unwrap();
        let from_snap = |id: SnapId| Some(id);
        let back = tree_from_snap(Some(&snap), &from_snap);
        match back.root.unwrap() {
            Node::Split {
                dir,
                ratios,
                children,
            } => {
                assert_eq!(dir, SplitDir::V);
                assert!((ratios[0] - 0.4).abs() < 1e-5);
                assert!((ratios[1] - 0.6).abs() < 1e-5);
                assert!(matches!(children[0], Node::Leaf(1)));
                match &children[1] {
                    Node::Split {
                        dir,
                        ratios,
                        children,
                    } => {
                        assert_eq!(*dir, SplitDir::H);
                        assert!((ratios[0] - 0.25).abs() < 1e-5);
                        assert!((ratios[1] - 0.75).abs() < 1e-5);
                        assert!(matches!(children[0], Node::Leaf(2)));
                        assert!(matches!(children[1], Node::Leaf(3)));
                    }
                    _ => panic!("expected nested split"),
                }
            }
            _ => panic!("expected root split"),
        }
    }

    #[test]
    fn empty_tree_round_trips() {
        let tree = LayoutTree { root: None };
        let to_snap = |id: WinId| id;
        assert!(tree_to_snap(&tree, &to_snap).is_none());
        let from_snap = |id: SnapId| Some(id);
        let back = tree_from_snap(None, &from_snap);
        assert!(back.root.is_none());
    }

    #[test]
    fn unmapped_split_collapses_to_none() {
        let snap = NodeSnap::Split {
            dir: "H".into(),
            ratios: vec![0.5, 0.5],
            children: vec![NodeSnap::Leaf { id: 1 }, NodeSnap::Leaf { id: 2 }],
        };
        let map = |_id: SnapId| None;
        let tree = tree_from_snap(Some(&snap), &map);
        assert!(tree.root.is_none());
    }
}
