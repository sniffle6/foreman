//! Cold workspace snapshot: types + serde + `%APPDATA%\foreman\workspace.json`
//! load/save. Capture/apply and dirty-flag wiring live in later tasks; this
//! module is pure data + I/O only.
//!
//! Mirrors `recents.rs` / `config.rs`: defaults in code, corruption-tolerant
//! load, atomic save via `config::save_json`. Future file versions are rejected
//! (not partially applied) so a newer foreman never leaves a half-restored tree.

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
    pub content: ContentSnap,
}

impl Default for TabSnap {
    fn default() -> Self {
        Self {
            title: String::new(),
            content: ContentSnap::Chat,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ContentSnap {
    Terminal { shell: String },
    Chat,
    Project { child: ManagerSnap },
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

/// Parse a shell string; unknown values degrade to PowerShell.
pub fn shell_from_str(s: &str) -> crate::terminal::Shell {
    match s {
        "powershell" => crate::terminal::Shell::PowerShell,
        "cmd" => crate::terminal::Shell::Cmd,
        "bash" => crate::terminal::Shell::Bash,
        _ => crate::terminal::Shell::PowerShell,
    }
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
        let s: WorkspaceSnapshot =
            serde_json::from_str(r#"{"version":1,"desktop":{}}"#).unwrap();
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
    fn unknown_fields_are_ignored() {
        let s: WorkspaceSnapshot = serde_json::from_str(
            r#"{"version":1,"desktop":{},"future_top":true}"#,
        )
        .unwrap();
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
}
