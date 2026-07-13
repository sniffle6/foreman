# Cold Workspace Persistence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** On restart, cold-restore the previous desktop layout (projects, nested tiling/tabs/floats/minimize/focus/zoom) with fresh shells at each project’s `cwd`, via `%APPDATA%\foreman\workspace.json`.

**Architecture:** New deep module `src/workspace.rs` owns the snapshot types, load/save (`config::load_json`/`save_json`), and pure tree conversion. `WindowManager` gains disk-free `capture_workspace` / `apply_workspace` (or equivalent) plus a structural dirty flag. `App` loads on first start, debounces saves (~600 ms), and flushes on clean quit. Panel prefs stay in `settings.json` only; `TaskManager` is never serialized.

**Tech Stack:** Rust, egui 0.34, serde/serde_json (already deps). No new crates.

**Spec:** `docs/superpowers/specs/2026-07-13-workspace-persistence-design.md` — read it first.

## Global Constraints

- Windows/PowerShell, GNU toolchain. Kill the app before linking:  
  `Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500`  
  (If `$env:FOREMAN` is `1`, do **not** kill foreman — use `cargo build --target-dir target/agent` instead.)
- Build/test: `cargo build 2>&1 | Select-Object -Last 20`; `cargo test --lib workspace`; `cargo test --lib layout`; `cargo test --lib wm`; full `cargo test` before claiming done.
- **No new dependencies.**
- `#[serde(default)]` on every snapshot struct; corrupt/missing file → empty default, never panic.
- Coordinates in the snapshot are **local** to the manager area (never screen space).
- `Content::TaskManager` is never written or restored; always recreated via `ensure_panel`.
- Terminals restore at **project `cwd` only** (not live shell cwd); no agent command re-dispatch.
- Runtime `WinId` / `tN` / `pN` regenerate on apply; snapshot uses `SnapId` only inside the file.
- Commit style: `type(scope): subject` + body why. Prefer a feature branch; do not bare-push to main.
- Trailer for agent commits: `Co-Authored-By: Claude <noreply@anthropic.com>` (or the session’s model trailer).

---

## File map

| File | Responsibility |
|---|---|
| **Create** `src/workspace.rs` | Snapshot types, serde, load/save, NodeSnap↔Node, Shell string map, pure helpers |
| **Modify** `src/main.rs` | `mod workspace;`; load/apply on first frame; debounce + quit flush |
| **Modify** `src/layout.rs` | Optional: `from_snap`/`to_snap` on `LayoutTree`/`Node` if conversion lives here |
| **Modify** `src/wm.rs` | `capture_workspace` / `apply_workspace`; dirty flag + mark sites; tests |
| **Create** `docs/workspace-persistence.md` | Operator-facing feature doc (Task 6) |
| **Touch** `docs/settings-persistence.md` | One paragraph: workspace is a separate file |

---

### Task 1: Snapshot types + serde + load/save — **Tier S**

**Files:**
- Create: `src/workspace.rs`
- Modify: `src/main.rs` (add `mod workspace;` near other mods, after `mod wm` or alphabetically consistent)

**Interfaces:**
- Consumes: `crate::config::{load_json, save_json}`, `serde`, `std::path::PathBuf`
- Produces:
  - `pub type SnapId = u64;`
  - `pub const WORKSPACE_FILE: &str = "workspace.json";`
  - `pub const WORKSPACE_VERSION: u32 = 1;`
  - `pub struct WorkspaceSnapshot { pub version: u32, pub desktop: ManagerSnap }` with `Default`, `Serialize`, `Deserialize`, `#[serde(default)]`
  - `pub struct ManagerSnap { pub cwd: Option<PathBuf>, pub focused: Option<SnapId>, pub last_focused: Option<SnapId>, pub zoomed: Option<SnapId>, pub windows: Vec<WinSnap>, pub tree: Option<NodeSnap> }`
  - `pub struct WinSnap { pub id: SnapId, pub active: usize, pub tabs: Vec<TabSnap>, pub minimized: bool, pub min_from_tree: bool, pub rect: RectSnap, pub prev: Option<RectSnap> }`
  - `pub struct TabSnap { pub title: String, pub content: ContentSnap }`
  - `pub enum ContentSnap { Terminal { shell: String }, Chat, Project { child: ManagerSnap } }`
  - `pub enum NodeSnap { Leaf { id: SnapId }, Split { dir: String, ratios: Vec<f32>, children: Vec<NodeSnap> } }`  // dir: `"H"` | `"V"`
  - `pub struct RectSnap { pub x: f32, pub y: f32, pub w: f32, pub h: f32 }`
  - `impl WorkspaceSnapshot { pub fn load() -> Self; pub fn save(&self) -> Result<(), String>; pub fn is_empty(&self) -> bool }`
  - `pub fn shell_to_str(shell: crate::terminal::Shell) -> &'static str` → `"powershell"` | `"cmd"` | `"bash"`
  - `pub fn shell_from_str(s: &str) -> crate::terminal::Shell` → unknown → `PowerShell`

**Notes:**
- `windows` z-order: **low index = back, high index = front** (spec).
- `is_empty`: true when `desktop.windows` has no project content after conceptual filter (for load, use: no `ContentSnap::Project` in any tab of any window). Implement as a method that walks content tags.
- `load()` must reject `version > WORKSPACE_VERSION`: return `WorkspaceSnapshot::default()` and `eprintln!` (do not fail the process). Missing version → treat as 1.

- [ ] **Step 1: Write failing tests in `src/workspace.rs`**

```rust
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
        assert!(s.desktop.windows.is_empty(), "future version must not partially load");
    }
}
```

Also export `pub fn parse_workspace_json(text: &str) -> WorkspaceSnapshot` that applies version policy + serde (used by `load()` after reading the file). This keeps version rejection unit-testable without touching `%APPDATA%`.

- [ ] **Step 2: Run tests — expect fail/compile error**

```powershell
cargo test --lib workspace -- --nocapture 2>&1 | Select-Object -Last 40
```

Expected: module missing or types undefined.

- [ ] **Step 3: Implement `src/workspace.rs`**

Implement all types with `#[derive(Clone, Debug, Default, Serialize, Deserialize)]` and `#[serde(default)]` on structs. For `ContentSnap` / `NodeSnap` use serde externally tagged or adjacently tagged — pick **internally tagged** with `"kind"` if external tagging is ugly for nested Project, e.g.:

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ContentSnap {
    Terminal { shell: String },
    Chat,
    Project { child: ManagerSnap },
}
```

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum NodeSnap {
    Leaf { id: SnapId },
    Split {
        dir: String,
        ratios: Vec<f32>,
        children: Vec<NodeSnap>,
    },
}
```

`Default` for `WorkspaceSnapshot`: `version: 0` or `1` and empty desktop — **pin in the empty_object test** so Default and `{}` agree (prefer `version: 1` on Default so empty saves are valid v1).

```rust
impl WorkspaceSnapshot {
    pub fn load() -> Self {
        let raw = /* read via load_json path OR: */
        // Prefer: load_json already returns Default on missing.
        // But version rejection needs parse after read — either:
        // 1) load_json::<WorkspaceSnapshot> then if version > WORKSPACE_VERSION { default }
        // 2) or custom read + parse_workspace_json
        let s: Self = crate::config::load_json(WORKSPACE_FILE);
        if s.version > WORKSPACE_VERSION {
            eprintln!(
                "foreman: workspace.json version {} is newer than supported {} — ignoring",
                s.version, WORKSPACE_VERSION
            );
            return Self::default();
        }
        // Missing version on old hand-written files: serde default 0 — treat 0 as 1:
        let mut s = s;
        if s.version == 0 {
            s.version = 1;
        }
        s
    }

    pub fn save(&self) -> Result<(), String> {
        crate::config::save_json(WORKSPACE_FILE, self)
    }

    pub fn is_empty(&self) -> bool {
        !self.desktop.has_project()
    }
}

impl ManagerSnap {
    pub fn has_project(&self) -> bool {
        self.windows.iter().any(|w| {
            w.tabs.iter().any(|t| matches!(t.content, ContentSnap::Project { .. }))
        })
    }
}
```

Wire `mod workspace;` in `main.rs`.

- [ ] **Step 4: Run tests — expect pass**

```powershell
cargo test --lib workspace -- --nocapture 2>&1 | Select-Object -Last 40
```

- [ ] **Step 5: Commit**

```powershell
git add src/workspace.rs src/main.rs
git commit -m "feat(workspace): snapshot types and workspace.json load/save"
```

---

### Task 2: Layout tree ↔ `NodeSnap` pure conversion — **Tier S**

**Files:**
- Modify: `src/workspace.rs` (conversion fns + tests)
- Optionally modify: `src/layout.rs` only if you prefer conversion next to `Node` — default is keep pure conversion in `workspace.rs` importing `crate::layout::{LayoutTree, Node, SplitDir}` and `crate::wm::WinId`

**Interfaces:**
- Consumes: `layout::{Node, LayoutTree, SplitDir}`, `wm::WinId`, `SnapId`
- Produces:
  - `pub fn node_to_snap(n: &Node, map: &dyn Fn(WinId) -> SnapId) -> NodeSnap`
  - `pub fn node_from_snap(n: &NodeSnap, map: &dyn Fn(SnapId) -> Option<WinId>) -> Option<Node>`  
    (returns `None` if a leaf id fails to map — caller drops that branch)
  - `pub fn tree_to_snap(tree: &LayoutTree, map: &dyn Fn(WinId) -> SnapId) -> Option<NodeSnap>`
  - `pub fn tree_from_snap(snap: Option<&NodeSnap>, map: &dyn Fn(SnapId) -> Option<WinId>) -> LayoutTree`
  - Ratio fixup: if ratios length mismatches children or sum ≈ 0, redistribute equal weights; clamp individual ratios ≥ `layout::MIN_RATIO` only when normalizing a full set (document in comment — do not invent new tree math; simple equal split on invalid is OK)

- [ ] **Step 1: Failing tests**

```rust
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
        Node::Split { dir, ratios, children } => {
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
```

Implement collapse: if after filtering children a split has 0 children → `None`; 1 child → that child; 2+ re-normalize ratios to sum 1.0.

- [ ] **Step 2: Run tests — expect fail**

```powershell
cargo test --lib workspace -- --nocapture 2>&1 | Select-Object -Last 30
```

- [ ] **Step 3: Implement conversion**

```rust
pub fn node_to_snap(n: &crate::layout::Node, map: &dyn Fn(WinId) -> SnapId) -> NodeSnap {
    match n {
        crate::layout::Node::Leaf(id) => NodeSnap::Leaf { id: map(*id) },
        crate::layout::Node::Split { dir, ratios, children } => NodeSnap::Split {
            dir: match dir {
                crate::layout::SplitDir::H => "H".into(),
                crate::layout::SplitDir::V => "V".into(),
            },
            ratios: ratios.clone(),
            children: children.iter().map(|c| node_to_snap(c, map)).collect(),
        },
    }
}

// node_from_snap + tree_* as specified above
```

`SplitDir` may be private — if so, either make it `pub` in `layout.rs` or match via existing public API. Prefer `pub use` / `pub enum SplitDir` if currently private (check compile).

- [ ] **Step 4: Tests pass**

```powershell
cargo test --lib workspace 2>&1 | Select-Object -Last 30
```

- [ ] **Step 5: Commit**

```powershell
git add src/workspace.rs src/layout.rs
git commit -m "feat(workspace): layout tree snapshot conversion"
```

---

### Task 3: Capture live `WindowManager` → `ManagerSnap` — **Tier S**

**Files:**
- Modify: `src/workspace.rs` (`capture_manager`, `rect_to_snap`)
- Modify: `src/wm.rs` (`pub fn capture_workspace(&self) -> crate::workspace::WorkspaceSnapshot` on desktop, or free fn taking `&WindowManager`)

**Interfaces:**
- Consumes: `WindowManager { windows, focused, last_focused, zoomed, tree, cwd, ... }`, `Content::{Terminal, Project, Chat, TaskManager}`, `Session.shell`
- Produces:
  - `workspace::capture_manager(wm: &WindowManager) -> ManagerSnap`
  - `WindowManager::capture_workspace(&self) -> WorkspaceSnapshot`  
    (`version: WORKSPACE_VERSION`, `desktop: capture_manager(self)`)

**Capture rules (spec):**
1. Assign each live `WinId` a `SnapId` — **use the live id as SnapId** for simplicity (u64); document that apply still allocates fresh runtime ids and remaps.  
2. Skip windows where **every** tab is `TaskManager`. If a window mixed TaskManager with other content (should not happen), skip only TaskManager tabs.  
3. Sort / order `windows` vec by ascending `z` so high z = front = high index.  
4. Terminal → `ContentSnap::Terminal { shell: shell_to_str(s.shell).into() }`  
5. Chat → `ContentSnap::Chat`  
6. Project → recurse `capture_manager(child)`  
7. `tree`: `tree_to_snap`, mapping only windows that were included  
8. `focused` / `last_focused` / `zoomed`: map if that id was included, else `None`  
9. `rect`/`prev` via `RectSnap { x: r.min.x, y: r.min.y, w: r.width(), h: r.height() }`

- [ ] **Step 1: Failing test in `wm.rs` tests (no PTY)**

Reuse stub windows. Build a desktop with two project stubs, one split, custom titles, one minimized floating:

```rust
#[test]
fn capture_workspace_skips_panel_and_records_tree() {
    let mut d = WindowManager::new().as_desktop();
    // panel-like window
    let pid = {
        let id = d.next;
        d.next += 1;
        d.z += 1;
        d.windows.push(Win {
            id,
            tabs: vec![Tab {
                title: "sessions".into(),
                content: Content::TaskManager(crate::panel::PanelView::new(
                    false,
                    crate::panel::PANEL_W,
                )),
            }],
            active: 0,
            rect: egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(260.0, 800.0)),
            z: d.z,
            minimized: false,
            min_from_tree: false,
            prev: None,
        });
        id
    };
    let a = push(&mut d, "proj-a");
    let b = push(&mut d, "proj-b");
    // Mark as projects with cwd (mutate stub content)
    for (id, cwd) in [(a, r"C:\a"), (b, r"C:\b")] {
        if let Some(w) = d.windows.iter_mut().find(|w| w.id == id) {
            if let Content::Project(child) = &mut w.tabs[0].content {
                child.cwd = Some(std::path::PathBuf::from(cwd));
            }
        }
    }
    d.tree = crate::layout::LayoutTree {
        root: Some(crate::layout::Node::Split {
            dir: crate::layout::SplitDir::H,
            ratios: vec![0.4, 0.6],
            children: vec![
                crate::layout::Node::Leaf(a),
                crate::layout::Node::Leaf(b),
            ],
        }),
    };
    // Panel not in tree for this test (or is — either way capture must omit panel win)
    d.focused = Some(b);

    let snap = d.capture_workspace();
    assert_eq!(snap.version, crate::workspace::WORKSPACE_VERSION);
    assert_eq!(snap.desktop.windows.len(), 2, "panel window omitted");
    assert!(snap.desktop.windows.iter().all(|w| w.id != pid));
    assert_eq!(snap.desktop.focused, Some(b));
    assert!(snap.desktop.tree.is_some());
}
```

If `PanelView` has no `Default`, construct the same way `ensure_panel` does — grep `PanelView` constructors and use that.

Add a second test: chat tab captures as `ContentSnap::Chat`:

```rust
#[test]
fn capture_records_chat_tab() {
    let mut m = WindowManager::new();
    m.cwd = Some(std::path::PathBuf::from(r"C:\p"));
    let id = m.next;
    m.next += 1;
    m.z += 1;
    m.windows.push(Win {
        id,
        tabs: vec![
            Tab {
                title: "chat".into(),
                content: Content::Chat(crate::chat::ChatView::new(std::rc::Rc::clone(&m.chat))),
            },
        ],
        active: 0,
        rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(100.0, 100.0)),
        z: m.z,
        minimized: false,
        min_from_tree: false,
        prev: None,
    });
    let snap = crate::workspace::capture_manager(&m);
    assert!(matches!(
        snap.windows[0].tabs[0].content,
        crate::workspace::ContentSnap::Chat
    ));
}
```

- [ ] **Step 2: Run — expect fail**

```powershell
cargo test --lib wm capture_workspace -- --nocapture 2>&1 | Select-Object -Last 40
```

- [ ] **Step 3: Implement capture**

```rust
// workspace.rs
pub fn capture_manager(wm: &crate::wm::WindowManager) -> ManagerSnap { ... }

// wm.rs
impl WindowManager {
    pub fn capture_workspace(&self) -> crate::workspace::WorkspaceSnapshot {
        crate::workspace::WorkspaceSnapshot {
            version: crate::workspace::WORKSPACE_VERSION,
            desktop: crate::workspace::capture_manager(self),
        }
    }
}
```

`capture_manager` needs access to private fields (`focused`, `tree`, `cwd`, …). Options:
1. Implement `capture_manager` as a method on `WindowManager` in `wm.rs` that builds `ManagerSnap` (workspace types public).  
2. Or add a `pub(crate)` getter bundle.

**Prefer method on `WindowManager`** (`fn capture_manager(&self) -> ManagerSnap`) in `wm.rs` to avoid exposing private fields; keep pure tree helpers in `workspace.rs`.

- [ ] **Step 4: Tests pass**

```powershell
cargo test --lib wm capture_ -- --nocapture 2>&1 | Select-Object -Last 40
cargo test --lib workspace 2>&1 | Select-Object -Last 20
```

- [ ] **Step 5: Commit**

```powershell
git add src/wm.rs src/workspace.rs
git commit -m "feat(workspace): capture live window manager to snapshot"
```

---

### Task 4: Apply snapshot → rebuild managers (spawn shells) — **Tier C**

**Files:**
- Modify: `src/wm.rs` (`apply_workspace`, helpers)
- Modify: `src/workspace.rs` only if pure helpers needed

**Interfaces:**
- Consumes: `ManagerSnap`, `egui::Context`, existing `Session::spawn` / `term_env` / `push_win` / `ChatView::new`
- Produces:
  - `WindowManager::apply_workspace(&mut self, snap: &WorkspaceSnapshot, ctx: &egui::Context) -> ApplyReport`
  - `struct ApplyReport { projects_restored: usize, projects_skipped: usize }` (for logs/tests)

**Apply algorithm (spec, implement exactly):**

```text
apply_manager(wm, snap, ctx):
  clear wm.windows, wm.tree, focus fields (caller ensures fresh or we replace)
  snap_id_to_win: HashMap<SnapId, WinId>

  for win_snap in snap.windows (in order):
    materialize tabs:
      for each TabSnap:
        Terminal { shell } -> Session::spawn(shell_from_str, wm.cwd, env, ctx) 
            on Ok: Content::Terminal(s) with set_term_id after id known
            on Err: skip tab (eprintln)
        Chat -> Content::Chat(ChatView::new(Rc::clone(&wm.chat)))
        Project { child } ->
          if child.cwd.as_ref().is_none_or(|p| !p.is_dir()) { skip tab; projects_skipped++ }
          else {
            let mut nested = WindowManager::new();
            nested.cwd = child.cwd.clone();
            // tag set after we know parent WinId
            apply_manager(&mut nested, child, ctx);
            Content::Project(Box::new(nested))
          }
    if no tabs survived: continue
    allocate runtime WinId via next_slot or next+=1
    record map snap.id -> runtime id
    push Win with rect from RectSnap, flags, active (clamped), z ascending
    if Content::Project: set child.tag = Some(format!("p{id}"))

  rebuild tree from snap.tree with map
  restore focused/last_focused/zoomed if mapped
  for Project children already applied recursively before parent push —
      careful order: materialize nested fully before push
```

**Important differences from `add_project`:**
- Do **not** call `add_project` (it always spawns one default terminal and pushes `opened` for recents). Apply should **not** pollute recents — either skip `opened.push` or drain after restore in App.
- Do **not** auto `tile_new`; tree comes from snapshot.
- Desktop `apply_workspace` starts from empty desktop (or clears non-panel windows). **Panel:** if a panel already exists from `ensure_panel`, either:
  - **A (recommended):** `apply` only replaces project windows; caller runs `ensure_panel` **after** apply; apply must not leave a panel in the snapshot (capture omitted it). On first start: create empty desktop → apply projects → `ensure_panel`.  
  - Clear `windows`/`tree` at start of apply on desktop, then `ensure_panel` after.

Startup order in Task 5 will be:

```text
desktop = new().as_desktop()
if snap has restorable projects:
  desktop.apply_workspace(&snap, ctx)
else:
  auto-project if !landing
desktop.ensure_panel(collapsed, width)
```

**Skipping missing dirs:** `path.is_dir()` once at apply time.

- [ ] **Step 1: Tests**

**Unit (structure without multi-project PTY where possible):**  
Apply a snapshot whose only project has `cwd` = a real temp dir (`tempfile` if already a dep — check Cargo.toml; if not, use `std::env::temp_dir().join(unique)` and `create_dir_all`) with one terminal tab. Use `egui::Context::default()`.

```rust
#[test]
fn apply_restores_project_cwd_and_one_shell() {
    let dir = std::env::temp_dir().join(format!("foreman-ws-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let ctx = egui::Context::default();
    let snap = /* build WorkspaceSnapshot with Project child cwd = dir, one Terminal powershell, tree leaf */;
    let mut d = WindowManager::new().as_desktop();
    let rep = d.apply_workspace(&snap, &ctx);
    assert_eq!(rep.projects_restored, 1);
    // find project, assert child.cwd == dir, at least one Terminal tab
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn apply_skips_missing_project_dir() {
    let ctx = egui::Context::default();
    let snap = /* project cwd = C:\foreman-ws-does-not-exist-xyz */;
    let mut d = WindowManager::new().as_desktop();
    let rep = d.apply_workspace(&snap, &ctx);
    assert_eq!(rep.projects_restored, 0);
    assert!(rep.projects_skipped >= 1);
    assert!(d.windows.iter().all(|w| !w.is_project()));
}
```

If spawn is flaky in CI-less local env, keep tests focused on skip-missing and “windows empty after full skip”; one spawn test is enough.

- [ ] **Step 2: Run — expect fail**

```powershell
cargo test --lib wm apply_ -- --nocapture 2>&1 | Select-Object -Last 50
```

- [ ] **Step 3: Implement apply**

Implement carefully around borrows: build `Vec<Win>` first, then assign `self.windows`, `self.tree`, focus fields.

When spawning terminal:

```rust
let env = self.term_env(self.next); // or id after allocation
let mut s = Session::spawn(shell, self.cwd.as_deref(), &env, ctx.clone())?;
// after id known:
s.set_term_id(id);
```

Match existing `add_terminal` for env/id stamping.

Clamp `active` to `tabs.len()-1`.

- [ ] **Step 4: Tests pass + `cargo test --lib wm` green**

```powershell
cargo test --lib wm 2>&1 | Select-Object -Last 50
```

- [ ] **Step 5: Commit**

```powershell
git add src/wm.rs src/workspace.rs
git commit -m "feat(workspace): apply snapshot rebuilds projects and shells"
```

---

### Task 5: Dirty flag, debounce, startup restore, quit flush — **Tier C**

**Files:**
- Modify: `src/wm.rs` (dirty flag)
- Modify: `src/main.rs` (load, debounce, quit)

**Interfaces:**
- `WindowManager` gains `workspace_dirty: bool` (or only on desktop — fine if all managers have it and only desktop is polled).
- `pub fn mark_workspace_dirty(&mut self)` — also recurse into project children if a nested mutation should dirty the desktop: **simpler approach:** only the desktop flag matters; nested mutations call up via returning a bool from `apply_acts` or set dirty on desktop from `App` when `desktop.take_workspace_dirty()` after `show`.

**Recommended dirty design:**

```rust
// On WindowManager:
workspace_dirty: bool,

pub fn mark_workspace_dirty(&mut self) {
    self.workspace_dirty = true;
}

pub fn take_workspace_dirty(&mut self) -> bool {
    let d = self.workspace_dirty;
    self.workspace_dirty = false;
    d
}

/// After show/apply_acts, OR dirty from nested projects:
pub fn poll_workspace_dirty(&mut self) -> bool {
    let mut dirty = self.take_workspace_dirty();
    for w in &mut self.windows {
        for t in &mut w.tabs {
            if let Content::Project(child) = &mut t.content {
                dirty |= child.poll_workspace_dirty();
            }
        }
    }
    dirty
}
```

Call `mark_workspace_dirty()` at the end of structural handlers in `apply_acts` and keyboard command paths that open/close/split/float/min/rename/focus/zoom/tab. Minimum set (must not miss open/close/split/tab/float/min):

| Site | Action |
|---|---|
| After applying `Act::Close`, `CloseTab`, `Merge`, `Untab`, `AddTerm`, `SetTab`, `Float`, `Min`, `Max`, `Focus` (optional but spec wants focus) | mark |
| `add_project` / `add_project_with_command` / `add_terminal` / `open_chat_window` | mark |
| Tree insert/remove paths used by drop and leader split/move | mark |
| Rename commit | mark |
| Zoom toggle | mark |

If exhaustive marking is hard in one pass: mark dirty on **any** successful `apply_acts` that processed a non-empty act list, plus keyboard commands that mutate without acts. Prefer slightly over-dirty (extra saves) over missing saves.

**App wiring:**

```rust
// App fields:
workspace_dirty_at: Option<Instant>,
// constant:
const WORKSPACE_SAVE_DEBOUNCE: Duration = Duration::from_millis(600);
```

**First-frame startup** (replace the current always-auto-project block in `App::ui` when `!self.started`):

```rust
if !self.started {
    // zoom opt-out ... existing ...
    let snap = workspace::WorkspaceSnapshot::load();
    let mut restored = false;
    if !snap.is_empty() {
        let rep = self.desktop.apply_workspace(&snap, &ctx);
        restored = rep.projects_restored > 0;
        if rep.projects_skipped > 0 {
            eprintln!(
                "foreman: restored {} project(s), skipped {}",
                rep.projects_restored, rep.projects_skipped
            );
        }
    }
    self.desktop
        .ensure_panel(self.settings.panel_collapsed, self.settings.panel_width);
    if !restored && !self.landing_enabled {
        let dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let nid = self.desktop.add_project(Shell::PowerShell, dir, &ctx);
        self.desktop.tile_new(nid, None);
    }
    let _ = self.desktop.take_opened(); // still discard auto-project from recents
    // Do not leave restore-induced dirty true on first frame:
    let _ = self.desktop.poll_workspace_dirty();
    self.started = true;
}
```

**End of frame save** (alongside font/panel settings debounce — can share timer or separate):

```rust
if self.desktop.poll_workspace_dirty() {
    self.workspace_dirty_at = Some(Instant::now());
}
if let Some(t) = self.workspace_dirty_at {
    if t.elapsed() >= WORKSPACE_SAVE_DEBOUNCE {
        let snap = self.desktop.capture_workspace();
        if let Err(e) = snap.save() {
            eprintln!("foreman: could not save workspace: {e}");
        }
        self.workspace_dirty_at = None;
    }
}
```

**Quit flush:** find where viewport close / quit confirm proceeds. Before process exit / when `force_quit` leads to close, call capture+save once. Also implement `eframe::App::on_exit` if available in this egui version:

```rust
fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
    let snap = self.desktop.capture_workspace();
    let _ = snap.save();
}
```

Verify `on_exit` exists on `eframe::App` in 0.34; if not, flush on the quit-accepted path in `ui` before `ViewportCommand::Close`.

- [ ] **Step 1: Unit test dirty poll**

```rust
#[test]
fn nested_mark_surfaces_on_desktop_poll() {
    let mut d = WindowManager::new().as_desktop();
    let id = push(&mut d, "p");
    // mark dirty on nested project manager
    if let Content::Project(child) = &mut d.windows[0].tabs[0].content {
        child.mark_workspace_dirty();
    }
    assert!(d.poll_workspace_dirty());
    assert!(!d.poll_workspace_dirty(), "take clears");
}
```

- [ ] **Step 2: Implement dirty + App wiring**

- [ ] **Step 3: Build**

```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue
cargo build 2>&1 | Select-Object -Last 30
cargo test --lib workspace --lib wm 2>&1 | Select-Object -Last 40
```

- [ ] **Step 4: Manual smoke (required for Tier C)**

1. Run `cargo run`, open 2 projects, split a terminal, float one, rename, wait 1s, restart → layout returns.  
2. Collapse panel, restart → panel still collapsed (settings path).  
3. Close all projects (or quit clean), restart with landing off → auto cwd project only, not old layout.  
4. Optionally kill process mid-session after debounce → prior layout returns.

- [ ] **Step 5: Commit**

```powershell
git add src/wm.rs src/main.rs
git commit -m "feat(workspace): debounce save and restore on startup"
```

---

### Task 6: Docs + config-axis note + full verification — **Tier S**

**Files:**
- Create: `docs/workspace-persistence.md`
- Modify: `docs/settings-persistence.md` (short note that layout is `workspace.json`, not settings)
- Optional: one line in `docs/HANDOFF.md` §5 clarifying cold restore vs daemon (only if editing HANDOFF is welcome — keep to one sentence)

**Feature doc shape** (match house style):

```markdown
# Workspace persistence

## What it does
Cold-restores the last desktop layout from `%APPDATA%\foreman\workspace.json`...

## What it does not
- Live PTY / agent process survival (daemon)
- Live shell cwd after cd
- Chat history

## Gotchas
- Missing project directories are skipped
- Panel size is settings.json
- Empty layout after close is saved

## Key files
- src/workspace.rs
- src/wm.rs (capture/apply, dirty)
- src/main.rs (debounce, startup)
```

- [ ] **Step 1: Write docs**

- [ ] **Step 2: Full test suite**

```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue
cargo test 2>&1 | Select-Object -Last 60
```

Expected: all green (or only pre-existing failures unrelated to this work — do not ship new failures).

- [ ] **Step 3: Commit**

```powershell
git add docs/workspace-persistence.md docs/settings-persistence.md
git commit -m "docs(workspace): cold restore feature doc"
```

---

## Self-review (plan vs spec)

| Spec requirement | Task |
|---|---|
| `workspace.json` + atomic load/save | 1 |
| Version / serde defaults / future version reject | 1 |
| Full layout fidelity (tree, tabs, float, min, focus, zoom) | 3–4 |
| Project cwd for terminals | 4 |
| Terminal + Chat only; no agent re-dispatch | 4 (ContentSnap has no command field) |
| TaskManager omitted; panel from settings | 3, 5 |
| Debounce 600 ms + quit flush | 5 |
| Missing dir skip | 4 |
| Empty workspace save | 5 (capture empty after close) |
| Corrupt never kills app | 1 |
| Tests serde / tree / capture / apply | 1–4 |
| Feature doc | 6 |
| Not daemon | documented non-goal |

**No intentional placeholders left.** Implementers may need to adapt exact line numbers and `PanelView` constructors to current code.

---

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-13-workspace-persistence.md`.

**Two execution options:**

1. **Subagent-Driven (recommended)** — fresh subagent per task, review between tasks  
2. **Inline Execution** — run tasks in this session with checkpoints  

Which approach?
